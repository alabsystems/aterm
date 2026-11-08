// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Transfer terminal evidence before removing a claim from the admission plane. These
//! files are protocol data, not permission to retire an executable or a Source use.

use super::*;
use crate::{CohortClosureRef, cohort_namespace::Namespace};
use std::sync::Arc;

#[path = "cohort_archive_slot.rs"]
mod slot;
use slot::Slot;

#[cfg(test)]
#[path = "cohort_terminal_model_tests.rs"]
mod terminal_model_tests;

#[path = "cohort_archive_envelope.rs"]
mod envelope;

#[path = "cohort_archive_history.rs"]
mod history;

#[cfg(test)]
#[path = "cohort_archive_envelope_tests.rs"]
mod envelope_tests;

const CLOSED: &str = "closed.json";
const RECEIPT: &str = "receipt.json";
const HISTORY: &str = "claim.noindex";
const COMPACTED: &str = "compacted.json";

// Share the SAME original directory description among its retained members;
// each file still owns its own original descriptor and all origin checks.
struct HeldFile {
    parent: Arc<File>,
    name: String,
    file: File,
    stamp: Stamp,
}

struct HeldDirectory {
    parent: Arc<File>,
    name: String,
    file: Arc<File>,
    identity: Value,
    origin: (u64, u64, u32, i64, i64, i64, i64),
}

struct Inventory {
    root: Arc<File>,
    origin: (u64, u64, u32, i64, i64, i64, i64),
    files: Vec<HeldFile>,
    directories: Vec<HeldDirectory>,
}

impl Inventory {
    fn capture(transaction: &Transaction) -> io::Result<Self> {
        let root = Arc::new(transaction.anchor.directory.try_clone()?);
        let origin = directory_origin(&root)?;
        let latest = &transaction
            .latest
            .as_ref()
            .ok_or_else(|| bad("terminal has no tip"))?
            .0;
        let mut inventory = Self {
            root: Arc::clone(&root),
            origin,
            files: Vec::new(),
            directories: Vec::new(),
        };
        for item in fd::Dir::read_from(root.as_fd())? {
            let item = item?;
            let name = item
                .file_name()
                .to_str()
                .map_err(|_| bad("non-UTF8 terminal member"))?;
            if matches!(name, "." | "..") {
                continue;
            }
            if matches!(name, HEADER | MUTATION | CLOSED) || name == latest.name {
                let file = open(&root, name, false, false)?;
                let (text, stamp) = bytes(&file, MAX_ENTRY)?;
                if stamp.mode() & 0o111 != 0 {
                    return Err(bad("terminal inventory contains executable data"));
                }
                if (name == HEADER && text != transaction.anchor.header)
                    || (name == MUTATION && text != "cohort mutation lock v1\n")
                    || (name == latest.name && (text != latest.text || stamp != latest.origin))
                {
                    return Err(bad(
                        "terminal inventory differs from its original protocol data",
                    ));
                }
                inventory.files.push(HeldFile {
                    parent: Arc::clone(&root),
                    name: name.into(),
                    file,
                    stamp,
                });
                continue;
            }
            let number = name
                .strip_prefix("epoch-")
                .and_then(|s| s.strip_suffix(".noindex"))
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|n| *n > 0)
                .ok_or_else(|| bad("unknown terminal member"))?;
            if name != format!("epoch-{number:016}.noindex") || inventory.directories.len() >= 2 {
                return Err(bad("unbounded terminal epoch inventory"));
            }
            let directory = Arc::new(open(&root, name, false, true)?);
            let before = directory_origin(&directory)?;
            let mut count = 0;
            for child in fd::Dir::read_from(directory.as_fd())? {
                let child = child?;
                let child = child
                    .file_name()
                    .to_str()
                    .map_err(|_| bad("non-UTF8 terminal epoch member"))?;
                if matches!(child, "." | "..") {
                    continue;
                }
                count += 1;
                if count > epoch::WINDOW {
                    return Err(bad("unbounded terminal epoch"));
                }
                let (file, text, stamp) = held_immutable(&directory, child)?;
                if child == "base.json" {
                    let value: Value = aterm_json::from_str(&text)
                        .map_err(|_| bad("unknown epoch base payload"))?;
                    if value.get("kind").and_then(Value::as_str) != Some("cohort-epoch-base") {
                        return Err(bad("unknown terminal base payload"));
                    }
                } else {
                    let ordinal = child
                        .strip_suffix(".json")
                        .and_then(|s| s.parse::<u64>().ok())
                        .filter(|n| *n > 0 && format!("{n:016}.json") == child)
                        .ok_or_else(|| bad("unknown terminal checkpoint name"))?;
                    if Capture::decode(&text)?.ordinal != ordinal {
                        return Err(bad("unknown terminal checkpoint payload"));
                    }
                }
                inventory.files.push(HeldFile {
                    parent: Arc::clone(&directory),
                    name: child.into(),
                    file,
                    stamp,
                });
            }
            if directory_origin(&directory)? != before {
                return Err(bad("terminal epoch changed across inventory"));
            }
            inventory.directories.push(HeldDirectory {
                parent: Arc::clone(&root),
                name: name.into(),
                identity: identity(&directory, true)?,
                file: directory,
                origin: before,
            });
        }
        inventory.recheck()?;
        Ok(inventory)
    }

    fn recheck(&self) -> io::Result<()> {
        if directory_origin(&self.root)? != self.origin {
            return Err(bad("terminal root changed across publication"));
        }
        for directory in &self.directories {
            if identity(
                &open(&directory.parent, &directory.name, false, true)?,
                true,
            )? != directory.identity
                || directory_origin(&directory.file)? != directory.origin
            {
                return Err(bad("terminal directory changed across publication"));
            }
        }
        for file in &self.files {
            if Stamp::read(&file.file)? != file.stamp
                || Stamp::read(&open(&file.parent, &file.name, false, false)?)? != file.stamp
            {
                return Err(bad("terminal member changed across publication"));
            }
        }
        Ok(())
    }

    fn value(&self) -> io::Result<Value> {
        let root = identity(&self.root, true)?;
        let mut files = std::collections::BTreeMap::new();
        for file in &self.files {
            let parent = identity(&file.parent, true)?;
            let directory = if parent == root {
                ""
            } else {
                &self
                    .directories
                    .iter()
                    .find(|d| d.identity == parent)
                    .ok_or_else(|| bad("inventory file has an unknown parent"))?
                    .name
            };
            files.insert((directory.to_owned(),file.name.clone()),aterm_json::json!({"directory":directory,"name":file.name,"stamp":file.stamp.value()}));
        }
        let directories = self
            .directories
            .iter()
            .map(|d| {
                (
                    d.name.clone(),
                    aterm_json::json!({"name":d.name,"identity":d.identity}),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        Ok(
            aterm_json::json!({"files":files.into_values().collect::<Vec<_>>(),"directories":directories.into_values().collect::<Vec<_>>()}),
        )
    }
}

/// The committed compaction intent carries the original finite inventory. An interrupted
/// prune may leave only a subset; a cold reaper never substitutes a newly captured stamp.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CapacityTerminal",
        action = "Compact",
        project = "aterm_capacity::ledger::cohort_ledger::checkpoint::archive::terminal_model_tests::project"
    )
)]
fn finish_compaction(
    slot: &mut Slot,
    intent: &envelope::CoveredCompaction,
    history: &mut history::History,
) -> io::Result<()> {
    slot.check()?;
    if let Some(root) = slot.history()?
        && intent.value().get("claim") != Some(&identity(&root, true)?)
    {
        return Err(bad("compaction claim directory changed identity"));
    }
    history.compact(slot, intent)
}

/// Publish only create-only protocol data. A valid interrupted same-file publication is
/// covered through its SAME parsed descriptor; mismatched data is never overwritten.
fn sealed(parent: &File, name: &str, text: &str) -> io::Result<(File, Stamp)> {
    if text.len() > MAX_ENTRY {
        return Err(bad("terminal receipt exceeds its encoded bound"));
    }
    let (file, stamp) = match open(parent, name, true, false) {
        Ok(mut file) => {
            file.write_all(text.as_bytes())?;
            fd::fchmod(&file, Mode::RUSR)?;
            let stamp = Stamp::read(&file)?;
            (file, stamp)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let (file, actual, stamp) = held_immutable(parent, name)?;
            if actual != text {
                return Err(bad(
                    "existing terminal receipt differs from its original intent",
                ));
            }
            (file, stamp)
        }
        Err(error) => return Err(error),
    };
    cover_frozen(parent, name, &file, text, &stamp)?;
    Ok((file, stamp))
}

fn closed_text(transaction: &Transaction, reference: &CohortClosureRef) -> io::Result<String> {
    let (capture, tip) = transaction
        .latest
        .as_ref()
        .ok_or_else(|| bad("terminal receipt has no committed tip"))?;
    aterm_json::to_string(&aterm_json::json!({"v":3u64,"kind":"cohort-terminal",
        "reference":reference.to_json(),"claim":identity(&transaction.anchor.directory,true)?,
        "anchor":transaction.anchor.header,"capture":aterm_json::from_str::<Value>(&capture.encode()?).map_err(|_|bad("invalid terminal capture"))?,
        "tip":tip.value()})).map_err(|_|bad("cannot encode terminal receipt"))
}

pub(super) fn active_closed(
    transaction: &Transaction,
    namespace: &Namespace,
    reference: &CohortClosureRef,
) -> io::Result<bool> {
    let Some((capture, _)) = &transaction.latest else {
        return Ok(false);
    };
    if transaction.pending.is_some()
        || !capture.retired
        || !transaction.matches_claim(reference.marker(), reference.guard(), namespace.nonce())?
    {
        return Ok(false);
    }
    let expected = closed_text(transaction, reference)?;
    let (held, text, stamp) = match held_immutable(&transaction.anchor.directory, CLOSED) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if text != expected {
        return Err(bad("terminal receipt was replaced or changed"));
    }
    cover_frozen(&transaction.anchor.directory, CLOSED, &held, &text, &stamp)?;
    Ok(true)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CapacityTerminal",
        action = "CoverTerminal",
        project = "aterm_capacity::ledger::cohort_ledger::checkpoint::archive::terminal_model_tests::project"
    )
)]
fn cover_terminal(
    transaction: &Transaction,
    reference: &CohortClosureRef,
) -> io::Result<(File, Stamp)> {
    let terminal = closed_text(transaction, reference)?;
    let result = sealed(&transaction.anchor.directory, CLOSED, &terminal)?;
    #[cfg(test)]
    boundary("terminal-covered")?;
    Ok(result)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CapacityTerminal",
        action = "CoverArchive",
        project = "aterm_capacity::ledger::cohort_ledger::checkpoint::archive::terminal_model_tests::project"
    )
)]
fn cover_archive(slot: &mut Slot, receipt: &str) -> io::Result<(File, Stamp)> {
    let result = slot.sealed(RECEIPT, receipt)?;
    slot.sync()?;
    #[cfg(test)]
    boundary("archive-covered")?;
    Ok(result)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CapacityTerminal",
        action = "Move",
        project = "aterm_capacity::ledger::cohort_ledger::checkpoint::archive::terminal_model_tests::project"
    )
)]
fn transfer_claim(
    transaction: &Transaction,
    slot: &mut Slot,
    inventory: &mut Inventory,
) -> io::Result<()> {
    inventory.origin = slot.move_claim(
        &transaction.anchor.parent,
        transaction.anchor.key(),
        &transaction.anchor.directory,
    )?;
    inventory.recheck()?;
    slot.sync()?;
    aterm_durable::durable(&transaction.anchor.parent)?;
    slot.check()?;
    #[cfg(test)]
    boundary("archive-moved")?;
    Ok(())
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CapacityTerminal",
        action = "CoverInventory",
        project = "aterm_capacity::ledger::cohort_ledger::checkpoint::archive::terminal_model_tests::project"
    )
)]
fn cover_inventory(
    slot: &mut Slot,
    value: &Value,
    receipt: &Value,
    receipt_stamp: &Stamp,
) -> io::Result<envelope::CoveredCompaction> {
    let covered = envelope::Compaction::publish(slot, value, receipt, receipt_stamp)?;
    #[cfg(test)]
    boundary("compaction-covered")?;
    Ok(covered)
}

pub(super) fn publish(
    transaction: &Transaction,
    namespace: &Namespace,
    guard: &RecordLock,
) -> io::Result<()> {
    let (capture, _) = transaction
        .latest
        .as_ref()
        .ok_or_else(|| bad("retirement has no committed generation"))?;
    if transaction.pending.is_some() || !capture.retired {
        return Err(bad("archive requires a committed terminal generation"));
    }
    capture.check(&transaction.anchor.directory, guard)?;
    let (_, journal) = Journal::decode(
        &capture.text,
        capture.name.ends_with(CLAIM),
        namespace.nonce(),
    )?;
    if journal.lifetime() != crate::cohort::Lifetime::Drained {
        return Err(bad("archive has no actual terminal scope"));
    }
    let m = guard.metadata()?;
    let reference = transaction.anchor.closure_ref(
        namespace.nonce(),
        ClaimGuardIdentity {
            device: m.dev(),
            inode: m.ino(),
        },
    )?;
    let terminal = closed_text(transaction, &reference)?;
    let (closed, closed_stamp) = cover_terminal(transaction, &reference)?;
    transaction.anchor.validate(&transaction.anchor.directory)?;
    capture.check(&transaction.anchor.directory, guard)?;
    let mut inventory = Inventory::capture(transaction)?;
    let retired = namespace.open_retired()?;
    let key = reference.key();
    let mut slot = Slot::prepare(&retired, &key)?;
    if slot.history()?.is_some() {
        return Err(bad(
            "active publisher found an already moved archive history",
        ));
    }
    let receipt=aterm_json::to_string(&aterm_json::json!({"v":3u64,"kind":"cohort-archive",
        "reference":reference.to_json(),"slot":slot.identity(),"slot_gate":slot.gate_stamp().value(),"active":identity(&transaction.anchor.parent,true)?,
        "claim":identity(&transaction.anchor.directory,true)?,"terminal":aterm_json::from_str::<Value>(&terminal).map_err(|_|bad("invalid terminal receipt"))?,
        "terminal_origin":closed_stamp.value(),"inventory":inventory.value()?})).map_err(|_|bad("cannot encode terminal archive"))?;
    let (receipt_file, receipt_stamp) = cover_archive(&mut slot, &receipt)?;
    inventory.recheck()?;
    if Stamp::read(&closed)? != closed_stamp {
        return Err(bad("original terminal receipt changed before transfer"));
    }
    transaction.anchor.validate(&transaction.anchor.directory)?;
    capture.check(&transaction.anchor.directory, guard)?;
    transfer_claim(transaction, &mut slot, &mut inventory)?;
    if identity(&open(slot.file(), HISTORY, false, true)?, true)?
        != identity(&transaction.anchor.directory, true)?
        || Stamp::read(&receipt_file)? != receipt_stamp
        || Stamp::read(&open(slot.file(), RECEIPT, false, false)?)? != receipt_stamp
    {
        return Err(bad(
            "archive transfer changed its retained original evidence",
        ));
    }
    inventory.recheck()?;
    capture.check(&transaction.anchor.directory, guard)?;
    let receipt_value =
        aterm_json::from_str::<Value>(&receipt).map_err(|_| bad("invalid own archive receipt"))?;
    let mut history = history::History::inspect(slot.history()?, &receipt_value, None)?;
    let compacted_value = aterm_json::json!({"v":3u64,"kind":"cohort-terminal-compaction",
        "reference":reference.to_json(),"slot":slot.identity(),"claim":identity(&transaction.anchor.directory,true)?,
        "receipt":receipt_stamp.value(),"inventory":inventory.value()?});
    let covered = cover_inventory(&mut slot, &compacted_value, &receipt_value, &receipt_stamp)?;
    inventory.recheck()?;
    namespace.validate()?;
    finish_compaction(&mut slot, &covered, &mut history)?;
    slot.sync()?;
    namespace.validate()
}

pub(super) fn query(
    namespace: &Namespace,
    reference: &CohortClosureRef,
    reap: bool,
) -> io::Result<CohortClaimState> {
    let active = namespace.open_active()?;
    let absent = || match fd::fstatat(&active, reference.key(), fd::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(false),
        Err(error) if error.is_noent() => Ok(true),
        Err(error) => Err(io::Error::from(error)),
    };
    if !absent()? {
        return Ok(CohortClaimState::Unknown);
    }
    let retired = namespace.open_retired()?;
    let inspected = match Slot::inspect(&retired, &reference.key()) {
        Ok(slot) => slot,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(CohortClaimState::Unknown);
        }
        Err(error) => return Err(error),
    };
    #[cfg(test)]
    terminal_model_tests::query_entry(namespace, reference, inspected.observed());
    let (file, text, stamp) = match inspected.read(RECEIPT) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(CohortClaimState::Unknown);
        }
        Err(error) => return Err(error),
    };
    let value: Value = aterm_json::from_str(&text).map_err(|_| bad("torn archive receipt"))?;
    let intrinsic = envelope::validate(namespace, reference, &inspected, &value, &stamp)?;
    namespace.validate()?;
    inspected.check()?;
    // A valid other original guard is Unknown. Intrinsic corruption is already refused;
    // neither outcome covers this query's metadata or repairs a history on its behalf.
    if intrinsic.reference != *reference {
        return Ok(CohortClaimState::Unknown);
    }
    if intrinsic.journal.lifetime() != crate::cohort::Lifetime::Drained {
        return Err(bad("archive receipt has no exact terminal scope"));
    }
    let mut history = intrinsic.history;
    let compaction = intrinsic.compaction;
    let mut slot = inspected.cover()?;
    slot.cover(RECEIPT, &file, &text, &stamp)?;
    history.recheck()?;
    let history_exists = match slot.history()? {
        Some(history) => {
            if value.get("claim") != Some(&identity(&history, true)?) {
                return Err(bad("archived claim directory was replaced"));
            }
            true
        }
        None => false,
    };
    match compaction {
        Some(compaction) => {
            let covered = compaction.cover(&slot)?;
            if reap {
                finish_compaction(&mut slot, &covered, &mut history)?
            }
        }
        None if !history_exists => {
            return Ok(CohortClaimState::Unknown);
        }
        None => {
            if reap {
                let compacted_value = aterm_json::json!({"v":3u64,"kind":"cohort-terminal-compaction",
                    "reference":reference.to_json(),"slot":slot.identity(),"claim":value.get("claim").ok_or_else(||bad("archive lacks claim identity"))?,
                    "receipt":stamp.value(),"inventory":value.get("inventory").ok_or_else(||bad("archive lacks original inventory"))?});
                let covered = cover_inventory(&mut slot, &compacted_value, &value, &stamp)?;
                finish_compaction(&mut slot, &covered, &mut history)?;
            }
        }
    };
    if Stamp::read(&file)? != stamp
        || Stamp::read(&open(slot.file(), RECEIPT, false, false)?)? != stamp
    {
        return Err(bad("archive changed across closure query"));
    }
    history.recheck()?;
    report_closed(namespace, reference, &active, &slot)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CapacityTerminal",
        action = "Query",
        project = "aterm_capacity::ledger::cohort_ledger::checkpoint::archive::terminal_model_tests::project"
    )
)]
fn report_closed(
    namespace: &Namespace,
    reference: &CohortClosureRef,
    active: &File,
    slot: &Slot,
) -> io::Result<CohortClaimState> {
    namespace.validate()?;
    slot.check()?;
    match fd::fstatat(active, reference.key(), fd::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => return Ok(CohortClaimState::Unknown),
        Err(error) if error.is_noent() => {}
        Err(error) => return Err(error.into()),
    }
    #[cfg(test)]
    terminal_model_tests::query_decision(namespace, reference, slot);
    Ok(CohortClaimState::Closed)
}

#[cfg(test)]
thread_local! {static INTERRUPT:std::cell::Cell<Option<&'static str>>=const {std::cell::Cell::new(None)};}

#[cfg(test)]
struct Replacement {
    parent: File,
    key: String,
    action: CoverAction,
    slot: Value,
    phase: &'static str,
}

#[cfg(test)]
enum CoverAction {
    Replace { prepared: String, detached: String },
    TransientEntries,
}

#[cfg(test)]
thread_local! {static REPLACE:std::cell::RefCell<Option<Replacement>>=const {std::cell::RefCell::new(None)};}

#[cfg(test)]
pub(super) fn replace_after_cover(
    namespace: &Namespace,
    reference: &CohortClosureRef,
    phase: &'static str,
) -> io::Result<std::path::PathBuf> {
    let parent = namespace.open_retired()?;
    let original = Slot::open(&parent, &reference.key())?;
    if original.history()?.is_some() {
        return Err(bad(
            "replacement control requires completed metadata-only history",
        ));
    }
    let nonce = aterm_uds::rand::hex_token::<8>()?;
    let prepared = format!("replacement-{nonce}.noindex");
    let detached = format!("detached-{nonce}.noindex");
    fd::mkdirat(&parent, prepared.as_str(), Mode::RWXU)?;
    let directory = open(&parent, &prepared, false, true)?;
    for name in [".slot.lock", RECEIPT, COMPACTED] {
        let (_, text, _) = original.read(name)?;
        let mut file = open(&directory, name, true, false)?;
        file.write_all(text.as_bytes())?;
        fd::fchmod(&file, Mode::RUSR)?;
        aterm_durable::durable(&file)?;
    }
    aterm_durable::durable(&directory)?;
    aterm_durable::durable(&parent)?;
    let expected = original.identity().clone();
    drop(original);
    REPLACE.with(|slot| {
        *slot.borrow_mut() = Some(Replacement {
            parent,
            key: reference.key(),
            action: CoverAction::Replace {
                prepared,
                detached: detached.clone(),
            },
            slot: expected,
            phase,
        })
    });
    Ok(namespace.retired().join(detached))
}

#[cfg(test)]
pub(super) fn transient_after_cover(
    namespace: &Namespace,
    reference: &CohortClosureRef,
    phase: &'static str,
) -> io::Result<()> {
    let parent = namespace.open_retired()?;
    let original = Slot::open(&parent, &reference.key())?;
    let expected = original.identity().clone();
    drop(original);
    REPLACE.with(|slot| {
        *slot.borrow_mut() = Some(Replacement {
            parent,
            key: reference.key(),
            action: CoverAction::TransientEntries,
            slot: expected,
            phase,
        })
    });
    Ok(())
}

#[cfg(test)]
pub(super) fn after_metadata_cover(parent: &File, name: &str, phase: &str) -> io::Result<()> {
    let take = REPLACE.with(|slot| -> io::Result<bool> {
        Ok(slot
            .borrow()
            .as_ref()
            .is_some_and(|replacement| name == RECEIPT && replacement.phase == phase)
            && slot.borrow().as_ref().is_some_and(|replacement| {
                identity(parent, true).is_ok_and(|id| id == replacement.slot)
            }))
    })?;
    if !take {
        return Ok(());
    }
    let replacement = REPLACE.with(|slot| slot.borrow_mut().take().expect("matched replacement"));
    match replacement.action {
        CoverAction::Replace { prepared, detached } => {
            fd::renameat_with(
                &replacement.parent,
                replacement.key.as_str(),
                &replacement.parent,
                detached.as_str(),
                RenameFlags::NOREPLACE,
            )?;
            fd::renameat_with(
                &replacement.parent,
                prepared.as_str(),
                &replacement.parent,
                replacement.key.as_str(),
                RenameFlags::NOREPLACE,
            )?;
        }
        CoverAction::TransientEntries => {
            let before = directory_origin(parent)?;
            let victim = parent.metadata()?;
            let tick = (victim.ctime(), victim.ctime_nsec());
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the archive filesystem clock to pass the untouched slot",
                || {
                    let probe = format!(".clock-{}", aterm_uds::rand::hex_token::<8>().unwrap());
                    fd::mkdirat(&replacement.parent, probe.as_str(), Mode::RWXU).unwrap();
                    let file = open(&replacement.parent, &probe, false, true).unwrap();
                    let stamp = file.metadata().unwrap();
                    let observed = (stamp.ctime(), stamp.ctime_nsec());
                    drop(file);
                    fd::unlinkat(&replacement.parent, probe.as_str(), fd::AtFlags::REMOVEDIR)
                        .unwrap();
                    (observed > tick).then_some(())
                },
            );
            assert_eq!(
                directory_origin(parent)?,
                before,
                "clock witness must leave the victim untouched"
            );
            let name = format!(".transient-{}", aterm_uds::rand::hex_token::<8>()?);
            fd::mkdirat(parent, name.as_str(), Mode::RWXU)?;
            fd::unlinkat(parent, name.as_str(), fd::AtFlags::REMOVEDIR)?;
            assert_ne!(
                directory_origin(parent)?,
                before,
                "actual create/remove changes directory origin while restoring its entries"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn with_gate<T>(
    namespace: &Namespace,
    reference: &CohortClosureRef,
    prepare: bool,
    action: impl FnOnce() -> T,
) -> io::Result<T> {
    let parent = namespace.open_retired()?;
    let _gate = if prepare {
        Slot::prepare(&parent, &reference.key())?
    } else {
        Slot::open(&parent, &reference.key())?
    };
    Ok(action())
}

#[cfg(test)]
pub(super) fn interrupt_preparation(stage: &'static str) {
    slot::interrupt_preparation(stage)
}

#[cfg(test)]
pub(super) fn interrupt(stage: &'static str) {
    INTERRUPT.with(|slot| slot.set(Some(stage)));
}

#[cfg(test)]
fn boundary(stage: &str) -> io::Result<()> {
    if INTERRUPT.with(|slot| {
        if slot.get() == Some(stage) {
            slot.set(None);
            true
        } else {
            false
        }
    }) {
        return Err(io::Error::other("injected terminal archive interruption"));
    }
    Ok(())
}
