// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Saved setup records aterm cannot parse, set aside (System State design §3.5). A record that
//! fails to parse when read without the profile lock, and again while holding it (no writer
//! can be part-way through its rename then), is corrupt for good. aterm keeps it beside itself
//! (`<name>.unreadable`, never deleted) and goes on as from a fresh record, which runs nothing a
//! person did not ask for:
//!
//! - every Mac preference group is left as is: what the lost record said had run is gone, so a
//!   Dock change or a Messages and FaceTime sign-out could otherwise run again;
//! - aterm's own permissions are set up, as on every Mac, unless a person's Stop or Cancel holds
//!   them;
//! - the vault and the repositories stay as the readable plan had them, or as a fresh install
//!   would have them (on an ALab Mac, set up).
//!
//! The run's history goes with it, and so does a Stop request no pass folded in, which named the
//! retired run. Its hold is kept in the plan, empty, and in the decision, stopped: an older
//! aterm's Cancel is that decision, and a Restore's decision stands, held for aterm's own
//! permissions too ([`Restored::held`]), so no bring-forward stages them over it. A fresh,
//! unadmitted progress checkpoint names the recovered plan for Stop without authorizing any
//! stage. With no hold, a decision that stopped setup stays and any other becomes pending, so
//! the next startup offers and admits the plan.
//!
//! Only a record that is not JSON, or not of this profile's shape, is damaged. One a newer aterm
//! wrote ([`NEWER_SETUP`]) is sound: while one is there nothing is set aside, since that aterm
//! keeps its setup. A run counter that does not parse is rebuilt ([`revision::repair`]); that
//! changes nothing a person sees. A marker that does not parse, while every record does, says
//! nothing of a hold: it is kept beside itself and written again, finished, with the hold the
//! records say; nothing is set aside again.
//!
//! Each step can be taken again with the same outcome, and the marker ([`MARKER`]) records one
//! under way with whether a Stop held and its reserved replacement revision, so a set-aside cut
//! short at any step finishes on the next try exactly as it would have: the corrupt records
//! kept, the marker, the plan, the decision, the run-number reservation, the Stop request, the
//! replacement progress, then the marker done.

use super::*;

#[cfg(test)]
mod access_revision_tests;

const PLAN: &str = "automatic-plan.json";
const PROGRESS: &str = "initial-setup.json";
const DECISION: &str = "first-run.json";

/// The records a snapshot of the setup reads.
const RECORDS: [&str; 3] = [PLAN, PROGRESS, DECISION];

/// The record of the last set-aside: the names it kept the corrupt records under, whether a
/// person's Stop or Cancel held aterm's own permissions, and whether it is still under way. Once
/// done, its presence is what System State's calm note reads until a person chooses
/// ([`forget_set_aside_setup`]).
const MARKER: &str = "set-aside.json";

/// What one set-aside did, for the log and the exceptions digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetAside {
    /// The names the corrupt records are kept under, in the state folder.
    pub kept: Vec<String>,
    /// The recovered plan's published checkpoint, for Stop only. No stage is admitted.
    pub stop_target: u64,
}

/// Set aside the saved setup records of this login that are corrupt for good, and go on as from
/// a fresh record (module docs). `Ok(None)`: every record parses (or is absent) and nothing is
/// under way. An error (a read that failed for another reason, the profile lock another setup
/// holds) changes nothing a later try does not finish: the caller tries again.
pub fn set_aside_unreadable_setup() -> Result<Option<SetAside>, String> {
    set_aside_at(&state_root()?, alab_mac, &mut |_| Ok(()))
}

/// [`set_aside_unreadable_setup`], reporting its exact published checkpoint for
/// Stop even when a later finalization fails. The observer grants no admission.
pub fn set_aside_unreadable_setup_observing(
    mut published: impl FnMut(u64),
) -> Result<Option<SetAside>, String> {
    set_aside_with(&state_root()?, alab_mac, &mut |_| Ok(()), &mut published)
}

/// [`set_aside_unreadable_setup`] in `root`, a test's scratch folder, on a Mac that is an ALab
/// Mac when `alab` says so.
#[cfg(any(test, debug_assertions))]
pub fn set_aside_unreadable_setup_at(root: &Path, alab: bool) -> Result<Option<SetAside>, String> {
    set_aside_at(root, || alab, &mut |_| Ok(()))
}

/// [`set_aside_unreadable_setup_observing`] at a test's scratch root.
#[cfg(any(test, debug_assertions))]
pub fn set_aside_unreadable_setup_at_observing(
    root: &Path,
    alab: bool,
    mut published: impl FnMut(u64),
) -> Result<Option<SetAside>, String> {
    set_aside_with(root, || alab, &mut |_| Ok(()), &mut published)
}

/// Scratch recovery with a caller's filesystem change after Plan/Decision
/// publication and before the actual checkpoint reservation.
#[cfg(any(test, debug_assertions))]
pub fn set_aside_unreadable_setup_at_observing_before_checkpoint(
    root: &Path,
    alab: bool,
    before: impl FnOnce(),
    mut published: impl FnMut(u64),
) -> Result<Option<SetAside>, String> {
    let mut before = Some(before);
    set_aside_with(
        root,
        || alab,
        &mut |at| {
            if at == Step::Revision
                && let Some(before) = before.take()
            {
                before();
            }
            Ok(())
        },
        &mut published,
    )
}

/// The actual recovery with an error after its final marker was published, as
/// when that publication's directory flush fails. Its checkpoint was observed.
#[cfg(any(test, debug_assertions))]
pub fn set_aside_unreadable_setup_at_observing_done_error(
    root: &Path,
    alab: bool,
    mut published: impl FnMut(u64),
) -> Result<Option<SetAside>, String> {
    set_aside_with(
        root,
        || alab,
        &mut |at| {
            if at == Step::DonePublished {
                Err("injected recovery finalization error".into())
            } else {
                Ok(())
            }
        },
        &mut published,
    )
}

/// A person chose since aterm set its records aside: System State's calm note about it goes.
/// A marker still under way is kept: its set-aside has steps left to finish.
pub fn forget_set_aside_setup() -> Result<(), String> {
    forget_at(&state_root()?)
}

pub(in crate::macos_doctor) fn forget_at(root: &Path) -> Result<(), String> {
    if !marker_at(root)?.is_some_and(|marker| !marker.under_way) {
        return Ok(());
    }
    match std::fs::remove_file(root.join(MARKER)) {
        Ok(()) => durable_directory(root).map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("{MARKER}: {error}")),
    }
}

/// Whether System State says aterm set its records aside: a finished set-aside no person has
/// chosen since ([`forget_set_aside_setup`]).
pub(super) fn shown(root: &Path) -> bool {
    marker_at(root).is_ok_and(|marker| marker.is_some_and(|marker| !marker.under_way))
}

/// A step of a set-aside, which a test can end it before, as a crash would.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Keep,
    Mark,
    Plan,
    Decision,
    Revision,
    Stop,
    Progress,
    Done,
    DonePublished,
}

fn set_aside_at(
    root: &Path,
    alab: impl FnOnce() -> bool,
    step: &mut dyn FnMut(Step) -> Result<(), String>,
) -> Result<Option<SetAside>, String> {
    set_aside_with(root, alab, step, &mut |_| {})
}

fn set_aside_with(
    root: &Path,
    alab: impl FnOnce() -> bool,
    step: &mut dyn FnMut(Step) -> Result<(), String>,
    published: &mut dyn FnMut(u64),
) -> Result<Option<SetAside>, String> {
    let Some((first, counter)) = survey(root)? else {
        return Ok(None);
    };
    let under_way =
        |marker: &Option<Marker>| marker.as_ref().is_some_and(|marker| marker.under_way);
    if first.is_empty() && !counter && !under_way(&marker_at(root)?) {
        return Ok(None);
    }
    // A fresh install's vault and repositories, asked before the lock: the probe of the ALab
    // vault can take a moment, and it is asked only when the plan itself is lost.
    let fresh_alab = first.contains(&PLAN) && alab();
    let Some(_lock) = HeldLock::try_take(root)? else {
        return Err("another setup is saving this Mac's setup records".into());
    };
    let Some((corrupt, _)) = survey(root)? else {
        return Ok(None);
    };
    revision::repair(root)?;
    let marker = marker_at(root)?;
    if corrupt.is_empty() && !under_way(&marker) {
        return Ok(None);
    }
    if corrupt.is_empty()
        && let Some(marker) = marker.as_ref().filter(|marker| marker.unreadable)
    {
        return rewrite_marker(root, marker);
    }
    if corrupt.contains(&PLAN) && !first.contains(&PLAN) {
        return Err("the saved setup plan changed while aterm read it".into());
    }

    step(Step::Keep)?;
    let mut kept = marker
        .as_ref()
        .map_or_else(Vec::new, |marker| marker.kept.clone());
    for name in &corrupt {
        let aside = keep_aside(root, name)?;
        if !kept.contains(&aside) {
            kept.push(aside);
        }
    }
    if marker.as_ref().is_some_and(|marker| marker.unreadable) {
        let aside = keep_aside(root, MARKER)?;
        if !kept.contains(&aside) {
            kept.push(aside);
        }
    }
    durable_directory(root).map_err(|error| error.to_string())?;

    // What the readable records still say.
    let plan = if corrupt.contains(&PLAN) {
        None
    } else {
        read_bytes(root, PLAN)?
            .map(|bytes| plan_from(&bytes))
            .transpose()?
    };
    let known_decision = if corrupt.contains(&DECISION) {
        None
    } else {
        Some(decision_record(root)?)
    };
    let decision = known_decision.map(|(decision, _)| decision);
    let progress = if corrupt.contains(&PROGRESS) {
        None
    } else {
        progress_file(root)?
    };
    if corrupt.is_empty()
        && let Some(marker) = marker.as_ref().filter(|marker| marker.under_way)
    {
        if !marker.bindings.is_empty() && !marker.bindings.contains(&checkpoint_binding(root)?) {
            retire_marker(root)?;
            return Ok(None);
        }
        if let Some(revision) = marker.revision
            && progress.is_some_and(|progress| progress.revision >= revision)
        {
            // The checkpoint already landed, or a later choice owns the records.
            // Replay neither its plan nor its decision: a Restore can change only
            // the decision, and a new choice can publish its plan before progress.
            let own = plan.as_ref().is_some_and(|(plan, modern)| {
                *modern
                    && progress
                        == Some(Progress {
                            revision,
                            ..Progress::for_plan(plan)
                        })
            }) && (!marker.bindings.is_empty() || decision == Some(Decision::Pending));
            if !own {
                retire_marker(root)?;
                return Ok(None);
            }
            published(revision);
            write_marker(
                root,
                &Marker {
                    under_way: false,
                    ..marker.clone()
                },
            )?;
            step(Step::DonePublished)?;
            return Ok(Some(SetAside {
                kept,
                stop_target: revision,
            }));
        }
    }
    step(Step::Mark)?;
    // Serialize this replacement with Stop publication. A request already
    // published is preserved in the marker and plan before it is consumed.
    let _stop_lock = HeldLock::take_named(root, STOP_REQUEST_LOCK)?;
    let stop = stop_request_at(root);
    let restored = known_decision.and_then(|(_, restored)| restored);
    let held = marker
        .as_ref()
        .is_some_and(|marker| marker.under_way && marker.held)
        || holds(decision, restored, progress, stop);
    let reserved = marker
        .as_ref()
        .filter(|marker| marker.under_way)
        .and_then(|marker| marker.revision);
    let recovered = recovered_plan(plan, held, fresh_alab);
    validate_plan(&recovered)?;
    let planned = plan_record(&recovered, false);
    let pending = decided(decision, restored, held);
    // Write ahead of both record renames. Their exact prefixes are the only
    // readable states a retry may replace, even before a revision is reserved.
    let bindings = checkpoint_prefixes(root, &planned, pending.as_ref())?;
    write_marker(
        root,
        &Marker {
            kept: kept.clone(),
            held,
            under_way: true,
            unreadable: false,
            revision: reserved,
            bindings,
        },
    )?;

    step(Step::Plan)?;
    write_json(&root.join(PLAN), &planned)?;

    step(Step::Decision)?;
    if let Some(pending) = pending {
        write_json(&root.join(DECISION), &pending)?;
    }

    step(Step::Revision)?;
    revision::recover(root, progress.map(|progress| progress.revision))?;
    let stop_target = match reserved {
        Some(revision) => revision,
        None => revision::next(root, 0)?,
    };
    revision::reserve(root, stop_target)?;
    let binding = checkpoint_binding(root)?;
    // Fix this transaction's checkpoint before publishing it. A retry after
    // publication or the final marker must not assign the replacement another run.
    write_marker(
        root,
        &Marker {
            kept: kept.clone(),
            held,
            under_way: true,
            unreadable: false,
            revision: Some(stop_target),
            bindings: vec![binding],
        },
    )?;

    step(Step::Stop)?;
    if let Some(request) = stop {
        settle_stop_request_locked(root, request, || Ok(()))?;
    }

    step(Step::Progress)?;
    save_progress_observing(
        root,
        Progress {
            revision: stop_target,
            ..Progress::for_plan(&recovered)
        },
        published,
    )?;

    step(Step::Done)?;
    write_marker(
        root,
        &Marker {
            kept: kept.clone(),
            held,
            under_way: false,
            unreadable: false,
            revision: Some(stop_target),
            bindings: vec![binding],
        },
    )?;
    step(Step::DonePublished)?;
    Ok(Some(SetAside { kept, stop_target }))
}

/// Whether the records hold aterm's own permissions: a Cancel, a Restore's decision a set-aside
/// held ([`Restored::held`]), a Stop asked for, or a run a person stopped.
fn holds(
    decision: Option<Decision>,
    restored: Option<Restored>,
    progress: Option<Progress>,
    stop: Option<StopRequest>,
) -> bool {
    decision == Some(Decision::Stopped(Stop::Cancelled))
        || restored.is_some_and(|restored| restored.held)
        || stop.is_some()
        || progress.is_some_and(|progress| progress.stopped)
}

/// The decision a set-aside writes over `decision` (`None`: it stays as it is). A `held` one
/// is stopped: a Cancel stays; what a Restore keeps stands as the Restore's own decision, held
/// ([`Restored::held`]), so the preferences it put back stay put back; any other becomes a
/// Cancel, which an older aterm reads too. With no hold a stopped decision stays, and any other
/// becomes pending, keeping what a Restore keeps.
fn decided(decision: Option<Decision>, restored: Option<Restored>, held: bool) -> Option<Value> {
    match (decision, restored) {
        (Some(Decision::Stopped(Stop::Cancelled)), _) => None,
        (decision, Some(restored)) if held => {
            (decision != Some(Decision::Stopped(Stop::Restored)) || !restored.held).then(|| {
                Restored {
                    held: true,
                    ..restored
                }
                .record(RestoredState::Restored)
            })
        }
        _ if held => Some(stamped(json!({ "profile": PROFILE, "state": "cancelled" }))),
        (Some(Decision::Stopped(_)), _) => None,
        (_, Some(restored)) => Some(restored.record(RestoredState::Pending)),
        (_, None) => Some(stamped(json!({ "profile": PROFILE, "state": "pending" }))),
    }
}

/// Every record parses and the marker does not: it says nothing of a hold. It is kept beside
/// itself and written again, finished, with the hold the records say; nothing is set aside.
fn rewrite_marker(root: &Path, marker: &Marker) -> Result<Option<SetAside>, String> {
    let mut kept = marker.kept.clone();
    let aside = keep_aside(root, MARKER)?;
    if !kept.contains(&aside) {
        kept.push(aside);
    }
    durable_directory(root).map_err(|error| error.to_string())?;
    let (decision, restored) = decision_record(root)?;
    let held = holds(
        Some(decision),
        restored,
        progress_file(root)?,
        stop_request_at(root),
    );
    write_marker(
        root,
        &Marker {
            kept,
            held,
            under_way: false,
            unreadable: false,
            revision: None,
            bindings: Vec::new(),
        },
    )?;
    Ok(None)
}

/// The plan aterm goes on with: no Mac preference group; aterm's own permissions unless a
/// person's Stop or Cancel `held` them, and then nothing at all; the vault and the repositories
/// as the readable plan had them, or as a fresh install has them (`fresh_alab`).
fn recovered_plan(readable: Option<(Plan, bool)>, held: bool, fresh_alab: bool) -> Plan {
    let readable = readable.map(|(plan, _)| plan);
    let mut selection = Selection::none();
    if !held {
        selection.set(Setting::Permissions, true);
        for setting in [Setting::Secrets, Setting::Repositories] {
            selection.set(
                setting,
                readable
                    .as_ref()
                    .map_or(fresh_alab, |plan| plan.selection.contains(setting)),
            );
        }
    }
    Plan {
        selection,
        vault_url: readable.map_or_else(
            || aterm_types::macos_setup::DEFAULT_VAULT_URL.into(),
            |plan| plan.vault_url,
        ),
    }
}

/// Whether one of the records at `root` is a newer aterm's ([`survey`]): a read of them that
/// failed says so ([`InitialSetupSnapshotError::Newer`]) even where another record beside it is
/// damaged, since no set-aside goes while that one is there.
pub(super) fn holds_newer(root: &Path) -> bool {
    matches!(survey(root), Ok(None))
}

/// The records of [`RECORDS`] that are there and are damaged ([`reading`]), and whether the run
/// counter is ([`revision::damaged`]); `None` while one of them is a newer aterm's
/// ([`NEWER_SETUP`]), which keeps its setup. A read that fails for another reason (a permission,
/// a disk error, a folder where a record should be) is an error: nothing says the record is
/// corrupt.
fn survey(root: &Path) -> Result<Option<(Vec<&'static str>, bool)>, String> {
    let counter = match revision::damaged(root) {
        Err(why) if setup_is_newer(&why) => return Ok(None),
        counter => counter?,
    };
    let mut corrupt = Vec::new();
    for name in RECORDS {
        if let Some(bytes) = read_bytes(root, name)? {
            match reading(name, &bytes) {
                Ok(()) => {}
                Err(why) if setup_is_newer(&why) => return Ok(None),
                Err(_) => corrupt.push(name),
            }
        }
    }
    Ok(Some((corrupt, counter)))
}

fn read_bytes(root: &Path, name: &str) -> Result<Option<Vec<u8>>, String> {
    match crate::macos_doctor::read_record_bytes(&root.join(name)) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{name}: {error}")),
    }
}

/// The exact repaired plan (including origin) and full decision at reservation.
/// Stop is separate: its newer restriction is merged under the Stop-record lock.
fn checkpoint_binding(root: &Path) -> Result<[u8; 32], String> {
    let plan = read_bytes(root, PLAN)?;
    let decision = read_bytes(root, DECISION)?;
    Ok(bind_records(plan.as_deref(), decision.as_deref()))
}

fn bind_records(plan: Option<&[u8]>, decision: Option<&[u8]>) -> [u8; 32] {
    let mut bytes = b"aterm.setup-recovery.checkpoint.v1\0".to_vec();
    for record in [plan, decision] {
        match record {
            None => bytes.push(0),
            Some(record) => {
                bytes.push(1);
                bytes.extend_from_slice(&(record.len() as u64).to_le_bytes());
                bytes.extend_from_slice(record);
            }
        }
    }
    aterm_digest::Sha256::digest(&bytes)
}

fn checkpoint_prefixes(
    root: &Path,
    planned: &Value,
    pending: Option<&Value>,
) -> Result<Vec<[u8; 32]>, String> {
    let plan = read_bytes(root, PLAN)?;
    let decision = read_bytes(root, DECISION)?;
    let planned = aterm_json::to_vec(planned).map_err(|error| error.to_string())?;
    let pending = pending
        .map(aterm_json::to_vec)
        .transpose()
        .map_err(|error| error.to_string())?;
    let mut bindings = vec![bind_records(plan.as_deref(), decision.as_deref())];
    for binding in [
        bind_records(Some(&planned), decision.as_deref()),
        bind_records(Some(&planned), pending.as_deref().or(decision.as_deref())),
    ] {
        if !bindings.contains(&binding) {
            bindings.push(binding);
        }
    }
    Ok(bindings)
}

fn retire_marker(root: &Path) -> Result<(), String> {
    std::fs::remove_file(root.join(MARKER)).map_err(|error| error.to_string())?;
    durable_directory(root).map_err(|error| error.to_string())
}

/// `bytes` read as record `name`, as its reader reads them: an error is damage, or a newer
/// aterm's record ([`NEWER_SETUP`]).
fn reading(name: &str, bytes: &[u8]) -> Result<(), String> {
    match name {
        PLAN => plan_from(bytes).map(|_| ()),
        PROGRESS => aterm_json::from_slice::<Value>(bytes)
            .map_err(|_| "invalid initial setup progress record".to_owned())
            .and_then(|value| parse_progress(&value))
            .map(|_| ()),
        _ => parse_decision(bytes).map(|_| ()),
    }
}

/// The plan in `bytes` and whether it is modern, as [`saved_plan_record`] reads it, with the
/// origin [`saved_plan_provisional`] reads.
fn plan_from(bytes: &[u8]) -> Result<(Plan, bool), String> {
    let value: Value = aterm_json::from_slice(bytes).map_err(|_| "invalid automatic setup plan")?;
    if value
        .get("provisional")
        .is_some_and(|provisional| provisional.as_bool().is_none())
    {
        return Err("invalid automatic setup plan origin".into());
    }
    parse_plan_record(&value)
}

/// Keep record `name` beside itself under a name no record has, and that name: the first of
/// `<name>.unreadable`, `<name>.unreadable.2`, … that is free or already holds these bytes, so
/// a set-aside taken again keeps one copy. A hard link keeps the very file; where the folder
/// cannot hold one, a copy flushed before its name is published.
pub(super) fn keep_aside(root: &Path, name: &str) -> Result<String, String> {
    let path = root.join(name);
    let bytes = crate::macos_doctor::read_record_bytes(&path)
        .map_err(|error| format!("{name}: {error}"))?;
    for n in 1..=99_u32 {
        let aside = if n == 1 {
            format!("{name}.unreadable")
        } else {
            format!("{name}.unreadable.{n}")
        };
        let target = root.join(&aside);
        match crate::macos_doctor::read_record_bytes(&target) {
            Ok(kept) if kept == bytes => return Ok(aside),
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{aside}: {error}")),
        }
        if std::fs::hard_link(&path, &target).is_err() {
            aterm_machines::port::write_atomic(&target, &bytes, 0o600)
                .map_err(|error| format!("{aside}: {error}"))?;
        }
        return Ok(aside);
    }
    Err(format!("{name}: too many unreadable copies kept"))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Marker {
    kept: Vec<String>,
    held: bool,
    under_way: bool,
    unreadable: bool,
    revision: Option<u64>,
    bindings: Vec<[u8; 32]>,
}

/// The marker, if one. An unreadable marker says nothing of a hold: what held is read again from
/// the records, where a held set-aside writes it ([`decided`]) before it lets the Stop request
/// go.
fn marker_at(root: &Path) -> Result<Option<Marker>, String> {
    let Some(bytes) = read_bytes(root, MARKER)? else {
        return Ok(None);
    };
    let marker = aterm_json::from_slice::<Value>(&bytes)
        .ok()
        .filter(|value| value["profile"].as_str() == Some(PROFILE))
        .and_then(|value| {
            Some(Marker {
                kept: value["kept"]
                    .as_array()?
                    .iter()
                    .map(|name| name.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()?,
                held: value["held"].as_bool()?,
                under_way: value["under_way"].as_bool()?,
                unreadable: false,
                revision: match value.get("revision") {
                    None | Some(Value::Null) => None,
                    Some(value) => Some(value.as_u64().filter(|revision| *revision != 0)?),
                },
                bindings: match value.get("binding") {
                    None | Some(Value::Null) => Vec::new(),
                    Some(value) if value.as_str().is_some() => vec![parse_binding(value)?],
                    Some(value) => {
                        let bindings = value.as_array()?;
                        if !(1..=3).contains(&bindings.len()) {
                            return None;
                        }
                        bindings
                            .iter()
                            .map(parse_binding)
                            .collect::<Option<Vec<_>>>()?
                    }
                },
            })
        })
        .unwrap_or(Marker {
            under_way: true,
            unreadable: true,
            ..Marker::default()
        });
    Ok(Some(marker))
}

fn write_marker(root: &Path, marker: &Marker) -> Result<(), String> {
    write_json(
        &root.join(MARKER),
        &json!({
            "profile": PROFILE,
            "kept": marker.kept,
            "held": marker.held,
            "under_way": marker.under_way,
            "revision": marker.revision,
            "binding": (!marker.bindings.is_empty()).then(|| marker.bindings.iter().map(|binding| aterm_secrets::b64::encode_padded(binding)).collect::<Vec<_>>()),
        }),
    )
}

fn parse_binding(value: &Value) -> Option<[u8; 32]> {
    value
        .as_str()
        .and_then(aterm_secrets::b64::decode_padded)?
        .try_into()
        .ok()
}

#[cfg(test)]
#[path = "set_aside_tests.rs"]
mod tests;
