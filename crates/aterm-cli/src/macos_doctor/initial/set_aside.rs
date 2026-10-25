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
    match std::fs::read(root.join(name)) {
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
    let bytes = std::fs::read(&path).map_err(|error| format!("{name}: {error}"))?;
    for n in 1..=99_u32 {
        let aside = if n == 1 {
            format!("{name}.unreadable")
        } else {
            format!("{name}.unreadable.{n}")
        };
        let target = root.join(&aside);
        match std::fs::read(&target) {
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
mod tests {
    use super::*;

    const CORRUPT: &[u8] = b"{\"profile\":";

    fn plan_with(settings: &[Setting]) -> Plan {
        let mut selection = Selection::none();
        for setting in settings {
            selection.set(*setting, true);
        }
        Plan {
            selection,
            vault_url: "http://office.local:7484/v1/team".into(),
        }
    }

    fn setting_up(root: &Path, plan: &Plan) {
        write_json(&root.join(PLAN), &plan_record(plan, false)).unwrap();
        write_json(
            &root.join(DECISION),
            &json!({ "profile": PROFILE, "state": "applied", "accounts_verified": true }),
        )
        .unwrap();
        std::fs::write(root.join(PROGRESS), CORRUPT).unwrap();
    }

    fn saved(root: &Path) -> Plan {
        plan_from(&std::fs::read(root.join(PLAN)).unwrap())
            .unwrap()
            .0
    }

    /// The outcome a set-aside leaves, for comparing one cut short and finished with one that
    /// ran through.
    fn outcome(root: &Path) -> Vec<(String, Option<Vec<u8>>)> {
        let mut names: Vec<String> = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| !name.ends_with(".lock"))
            .collect();
        names.sort();
        names
            .into_iter()
            .map(|name| {
                let bytes = std::fs::read(root.join(&name)).ok();
                (name, bytes)
            })
            .collect()
    }

    /// A progress record that does not parse is kept beside itself and the setup goes on as
    /// from a fresh record: the Mac preference groups left as is, aterm's own permissions set
    /// up, the vault and repositories as the plan had them, the decision pending so the next
    /// startup admits the plan by itself, and a fresh checkpoint that only Stop can use.
    #[test]
    fn a_corrupt_progress_record_is_kept_aside_and_setup_goes_on_from_a_fresh_one() {
        let root = aterm_tempfile::tempdir().unwrap();
        let root = root.path();
        let chosen = plan_with(&[
            Setting::Dock,
            Setting::Messages,
            Setting::Permissions,
            Setting::Secrets,
        ]);
        setting_up(root, &chosen);

        let set = set_aside_unreadable_setup_at(root, false).unwrap().unwrap();
        assert_eq!(set.kept, ["initial-setup.json.unreadable"]);
        assert_eq!(
            std::fs::read(root.join("initial-setup.json.unreadable")).unwrap(),
            CORRUPT
        );
        let recovered = saved(root);
        assert_eq!(
            recovered,
            plan_with(&[Setting::Permissions, Setting::Secrets])
        );
        assert_eq!(decision_at(root).unwrap(), Decision::Pending);
        assert_eq!(
            progress_file(root).unwrap(),
            Some(Progress {
                revision: set.stop_target,
                ..Progress::for_plan(&recovered)
            })
        );
        assert!(admission_at(root).unwrap().is_none());
        assert!(!saved_plan_provisional(root).unwrap(), "never widened");
        assert!(shown(root));
        // Taken again, it finds nothing to do.
        assert_eq!(set_aside_unreadable_setup_at(root, false).unwrap(), None);
        // A person's choice since takes the note away.
        forget_at(root).unwrap();
        assert!(!shown(root));
        assert!(root.join("initial-setup.json.unreadable").exists(), "kept");
    }

    /// A lost plan is replaced by a fresh install's: the vault and repositories set up on an
    /// ALab Mac only; a lost decision becomes pending.
    #[test]
    fn a_lost_plan_or_decision_goes_on_as_a_fresh_install_would() {
        for alab in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let root = root.path();
            std::fs::write(root.join(PLAN), CORRUPT).unwrap();
            std::fs::write(root.join(DECISION), b"[]").unwrap();
            let set = set_aside_unreadable_setup_at(root, alab).unwrap().unwrap();
            assert_eq!(
                set.kept,
                [
                    "automatic-plan.json.unreadable",
                    "first-run.json.unreadable"
                ]
            );
            let mut fresh = vec![Setting::Permissions];
            if alab {
                fresh.extend([Setting::Secrets, Setting::Repositories]);
            }
            let recovered = saved(root);
            assert_eq!(recovered.selection, plan_with(&fresh).selection, "{alab}");
            assert_eq!(
                recovered.vault_url,
                aterm_types::macos_setup::DEFAULT_VAULT_URL
            );
            assert_eq!(decision_at(root).unwrap(), Decision::Pending);
        }
    }

    /// A person's Stop or Cancel holds aterm's own permissions through a set-aside: the plan it
    /// goes on with sets up nothing and the decision stands stopped, so no launch offers it, and
    /// the Stop request that named the lost run goes, so it never stops a later run that takes
    /// its number.
    #[test]
    fn a_stop_or_cancel_holds_through_a_set_aside() {
        for cancelled in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let root = root.path();
            setting_up(root, &plan_with(&[Setting::Permissions, Setting::Dock]));
            if cancelled {
                write_json(
                    &root.join(DECISION),
                    &json!({ "profile": PROFILE, "state": "cancelled" }),
                )
                .unwrap();
            } else {
                request_stop_at(root, 1).unwrap();
            }
            set_aside_unreadable_setup_at(root, true).unwrap().unwrap();
            assert!(saved(root).selection.is_empty(), "{cancelled}");
            assert!(stop_request_at(root).is_none());
            assert_eq!(
                decision_at(root).unwrap(),
                Decision::Stopped(Stop::Cancelled),
                "{cancelled}"
            );
            assert!(finished_at(root).unwrap());
        }
    }

    #[test]
    fn a_stop_after_an_interrupted_recovery_holds_its_replacement_plan() {
        let root = aterm_tempfile::tempdir().unwrap();
        let root = root.path();
        setting_up(root, &plan_with(&[Setting::Permissions, Setting::Secrets]));
        assert!(
            set_aside_at(root, || false, &mut |at| {
                if at == Step::Plan {
                    Err("the process ended after saving its marker".into())
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        assert!(!marker_at(root).unwrap().unwrap().held);
        request_stop_at(root, 3).unwrap();

        set_aside_unreadable_setup_at(root, false).unwrap().unwrap();
        assert!(saved(root).selection.is_empty(), "the later Stop holds");
        assert!(marker_at(root).unwrap().unwrap().held);
        assert!(stop_request_at(root).is_none());
        assert_eq!(
            decision_at(root).unwrap(),
            Decision::Stopped(Stop::Cancelled)
        );
        assert_eq!(set_aside_unreadable_setup_at(root, false).unwrap(), None);
    }

    #[test]
    fn a_stop_before_the_recovery_marker_is_saved_holds_its_replacement_plan() {
        let root = aterm_tempfile::tempdir().unwrap();
        let root = root.path();
        setting_up(root, &plan_with(&[Setting::Permissions, Setting::Secrets]));
        set_aside_at(root, || false, &mut |at| {
            if at == Step::Mark {
                request_stop_at(root, 3)?;
            }
            Ok(())
        })
        .unwrap()
        .unwrap();
        assert!(saved(root).selection.is_empty(), "the later Stop holds");
        assert!(marker_at(root).unwrap().unwrap().held);
        assert!(stop_request_at(root).is_none());
        assert_eq!(
            decision_at(root).unwrap(),
            Decision::Stopped(Stop::Cancelled)
        );
    }

    #[test]
    fn a_damaged_recovery_marker_cannot_release_a_recorded_stop() {
        for damaged in [
            CORRUPT.to_vec(),
            aterm_json::to_vec(&json!({
                "profile": PROFILE, "kept": [], "held": false,
                "under_way": true, "binding": ["not a checkpoint"],
            }))
            .unwrap(),
        ] {
            damaged_marker_holds(&damaged);
        }
    }

    fn damaged_marker_holds(damaged: &[u8]) {
        let root = aterm_tempfile::tempdir().unwrap();
        let root = root.path();
        setting_up(root, &plan_with(&[Setting::Permissions, Setting::Secrets]));
        request_stop_at(root, 3).unwrap();
        assert!(
            set_aside_at(root, || false, &mut |at| {
                if at == Step::Done {
                    Err("the process ended before finishing its marker".into())
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        assert!(saved(root).selection.is_empty());
        assert!(stop_request_at(root).is_none());
        assert!(!progress_file(root).unwrap().unwrap().admitted);
        std::fs::write(root.join(MARKER), damaged).unwrap();

        // Every record parses: the marker is written again, finished, holding what they hold.
        assert_eq!(set_aside_unreadable_setup_at(root, false).unwrap(), None);
        assert!(saved(root).selection.is_empty(), "the earlier Stop holds");
        let marker = marker_at(root).unwrap().unwrap();
        assert!(marker.held && !marker.under_way);
        assert_eq!(
            decision_at(root).unwrap(),
            Decision::Stopped(Stop::Cancelled)
        );
        assert_eq!(
            std::fs::read(root.join("set-aside.json.unreadable")).unwrap(),
            damaged,
            "the damaged marker is kept before replacement"
        );
    }

    /// A read that fails for another reason than a parse says nothing of the record: it is an
    /// error the caller tries again, and nothing is set aside.
    #[test]
    fn a_record_that_cannot_be_read_just_now_is_never_set_aside() {
        let root = aterm_tempfile::tempdir().unwrap();
        let root = root.path();
        std::fs::create_dir(root.join(DECISION)).unwrap();
        std::fs::write(root.join(PROGRESS), CORRUPT).unwrap();
        assert!(set_aside_unreadable_setup_at(root, false).is_err());
        assert!(!root.join("initial-setup.json.unreadable").exists());
        assert_eq!(std::fs::read(root.join(PROGRESS)).unwrap(), CORRUPT);
    }

    #[test]
    fn a_recovered_checkpoint_carries_no_old_stage_or_permission_authority() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        let old_plan = plan_with(&[Setting::Dock, Setting::Permissions, Setting::Secrets]);
        write_json(&root.join(PLAN), &plan_record(&old_plan, false)).unwrap();
        save_progress(
            root,
            Progress {
                revision: 8,
                access: Some(3),
                done: Stage::KNOWN,
                completed: true,
                admitted: true,
                ..Progress::for_plan(&old_plan)
            },
        )
        .unwrap();
        std::fs::write(root.join(DECISION), CORRUPT).unwrap();

        let set = set_aside_unreadable_setup_at(root, false).unwrap().unwrap();
        let recovered = saved(root);
        let staged = progress_file(root).unwrap().unwrap();
        assert_eq!(
            recovered,
            plan_with(&[Setting::Permissions, Setting::Secrets])
        );
        assert!(set.stop_target > 8);
        assert_eq!(
            staged,
            Progress {
                revision: set.stop_target,
                ..Progress::for_plan(&recovered)
            }
        );
        assert_eq!(access_revision_at(root).unwrap(), set.stop_target);
        assert!(admission_at(root).unwrap().is_none());
        assert!(!finished_at(root).unwrap());

        let admitted = InitialSetupAuthorization::capture_at(root)
            .unwrap()
            .persist_at(root, &recovered)
            .result
            .unwrap()
            .expect("without Stop the recovered plan is offered and admitted");
        assert!(admitted > set.stop_target);
        assert_eq!(admission_at(root).unwrap().unwrap().revision, admitted);
    }

    #[test]
    fn a_recovered_checkpoint_stop_holds_relaunch_but_not_a_later_manual_choice() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        setting_up(
            root,
            &plan_with(&[Setting::Dock, Setting::Permissions, Setting::Secrets]),
        );
        let set = set_aside_unreadable_setup_at(root, false).unwrap().unwrap();
        let recovered = saved(root);
        request_stop_at(root, set.stop_target).unwrap();

        let snapshot = snapshot_at(root, Plan::default()).unwrap();
        assert!(snapshot.finished && !snapshot.permissions_allowed);
        assert_eq!(snapshot.plan, recovered);
        assert_eq!(saved(root), recovered, "Stop preserves every saved group");
        assert!(
            InitialSetupAuthorization::capture_at(root)
                .unwrap()
                .persist_at(root, &recovered)
                .result
                .unwrap()
                .is_none(),
            "a fresh startup cannot admit the stopped checkpoint"
        );

        let later = observe_mutation_at(root, None, Err("busy".into()), |root| {
            InitialSetupRestart::chosen().persist_with(
                root,
                &recovered,
                false,
                || Ok(None),
                save_progress,
                |root| observation_at(root).map_err(|error| error.to_string()),
            )
        })
        .result
        .unwrap()
        .expect("a later manual choice has independent authority");
        assert!(later > set.stop_target);
        // A delayed old saver still names only the recovered checkpoint.
        request_stop_at(root, set.stop_target).unwrap();
        assert!(!finished_at(root).unwrap());
        assert_eq!(admission_at(root).unwrap().unwrap().revision, later);
        assert_eq!(saved(root), recovered);
    }

    fn interrupt_recovery(root: &Path, cut: Step) -> Option<u64> {
        let prior_plan = plan_with(&[Setting::Permissions, Setting::Secrets]);
        setting_up(root, &prior_plan);
        if cut != Step::Done {
            save_progress(
                root,
                Progress {
                    revision: 8,
                    admitted: true,
                    ..Progress::for_plan(&prior_plan)
                },
            )
            .unwrap();
            if cut == Step::Decision {
                std::fs::write(root.join(PLAN), CORRUPT).unwrap();
            } else {
                std::fs::write(root.join(DECISION), CORRUPT).unwrap();
            }
        }
        assert!(
            set_aside_at(root, || false, &mut |step| {
                if step == cut {
                    Err("the recovery process ended".into())
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        marker_at(root).unwrap().unwrap().revision
    }

    fn newer_choice_during_recovery(cut: Step, interrupted: bool) {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        let old_target = interrupt_recovery(root, cut);
        let chosen = plan_with(&[Setting::Dock]);
        let result = observe_mutation_at(root, None, Err("busy".into()), |root| {
            if interrupted {
                stage_restart_with(
                    root,
                    &chosen,
                    Restart {
                        chosen: true,
                        joins: false,
                    },
                    AdmittedVault::None,
                    save_progress,
                    |step| {
                        if step == RestartWrite::Progress {
                            Err("the later choice ended after its plan publication".into())
                        } else {
                            Ok(())
                        }
                    },
                )
                .map(|progress| Some(progress.revision))
            } else {
                InitialSetupRestart::chosen().persist_with(
                    root,
                    &chosen,
                    false,
                    || Ok(None),
                    save_progress,
                    |root| observation_at(root).map_err(|error| error.to_string()),
                )
            }
        })
        .result;
        assert_eq!(result.is_err(), interrupted);
        let current = progress_file(root).unwrap().unwrap();
        if interrupted {
            if cut == Step::Done {
                assert_eq!(Some(current.revision), old_target);
            } else {
                assert_eq!(current.revision, 8);
            }
            assert!(!current.belongs_to(&chosen));
        } else {
            assert!(current.revision > old_target.unwrap_or(8) && current.admitted);
        }
        assert!(marker_at(root).unwrap().unwrap().under_way);
        let records = || {
            [PLAN, PROGRESS, DECISION, "run-revision.json"]
                .map(|name| std::fs::read(root.join(name)).ok())
        };
        let before = records();

        assert!(
            set_aside_unreadable_setup_at(root, false)
                .unwrap()
                .is_none()
        );
        assert_eq!(saved(root), chosen, "the person's later selection survives");
        assert_eq!(
            records(),
            before,
            "retired recovery cannot rewrite its successor"
        );
    }

    #[test]
    fn an_interrupted_recovery_does_not_overwrite_a_later_admitted_choice() {
        newer_choice_during_recovery(Step::Done, false);
    }

    #[test]
    fn an_interrupted_recovery_does_not_overwrite_a_later_partially_saved_choice() {
        newer_choice_during_recovery(Step::Done, true);
    }

    #[test]
    fn an_unpublished_recovery_does_not_overwrite_a_later_partially_saved_choice() {
        newer_choice_during_recovery(Step::Progress, true);
    }

    #[test]
    fn an_unreserved_recovery_does_not_overwrite_a_later_partially_saved_choice() {
        newer_choice_during_recovery(Step::Revision, true);
    }

    #[test]
    fn an_unfinished_recovery_decision_does_not_overwrite_a_later_partially_saved_choice() {
        newer_choice_during_recovery(Step::Decision, true);
    }

    #[test]
    fn an_interrupted_recovery_does_not_overwrite_a_later_restore_decision() {
        for cut in [Step::Revision, Step::Done] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let root = scratch.path();
            interrupt_recovery(root, cut);
            let progress_before = progress_file(root).unwrap();
            observe_mutation_at(root, None, Err("busy".into()), save_restore)
                .result
                .unwrap();
            assert_eq!(progress_file(root).unwrap(), progress_before);
            let before = [PLAN, PROGRESS, DECISION, "run-revision.json"]
                .map(|name| std::fs::read(root.join(name)).ok());

            assert!(
                set_aside_unreadable_setup_at(root, false)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                [PLAN, PROGRESS, DECISION, "run-revision.json"]
                    .map(|name| std::fs::read(root.join(name)).ok()),
                before,
                "the full Restore decision and its plan survive: {cut:?}"
            );
        }
    }

    #[test]
    fn a_published_recovery_target_is_reported_even_when_done_returns_an_error() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        setting_up(root, &plan_with(&[Setting::Permissions, Setting::Secrets]));
        let mut target = None;
        assert!(
            set_aside_unreadable_setup_at_observing_done_error(root, false, |revision| {
                target = Some(revision);
            })
            .is_err()
        );
        let target = target.expect("Stop evidence precedes the failed finalization result");
        assert_eq!(progress_file(root).unwrap().unwrap().revision, target);
        assert!(!marker_at(root).unwrap().unwrap().under_way);
        assert!(
            set_aside_unreadable_setup_at_observing(root, false, |_| {
                panic!("a later retry cannot adopt an earlier repair's target")
            })
            .unwrap()
            .is_none()
        );
        let plan = saved(root);
        request_stop_at(root, target).unwrap();
        assert!(finished_at(root).unwrap());
        assert_eq!(saved(root), plan);
        assert!(
            InitialSetupAuthorization::capture_at(root)
                .unwrap()
                .persist_at(root, &plan)
                .result
                .unwrap()
                .is_none()
        );
    }

    /// A set-aside cut short before any step finishes on the next try exactly as one that ran
    /// through, the hold it found included, though the Stop request it found is gone by then.
    #[test]
    fn a_set_aside_cut_short_at_any_step_finishes_as_one_that_ran_through() {
        let steps = [
            Step::Keep,
            Step::Mark,
            Step::Plan,
            Step::Decision,
            Step::Revision,
            Step::Stop,
            Step::Progress,
            Step::Done,
        ];
        for stopped in [false, true] {
            let start = |root: &Path| {
                std::fs::write(root.join(PLAN), CORRUPT).unwrap();
                write_json(
                    &root.join(DECISION),
                    &json!({ "profile": PROFILE, "state": "applied", "accounts_verified": true }),
                )
                .unwrap();
                std::fs::write(root.join(PROGRESS), b"not json").unwrap();
                if stopped {
                    request_stop_at(root, 3).unwrap();
                }
            };
            let through = aterm_tempfile::tempdir().unwrap();
            start(through.path());
            set_aside_at(through.path(), || true, &mut |_| Ok(()))
                .unwrap()
                .unwrap();
            let expected = outcome(through.path());
            for cut in steps {
                let root = aterm_tempfile::tempdir().unwrap();
                let root = root.path();
                start(root);
                let crashed = set_aside_at(root, || true, &mut |at| {
                    if at == cut {
                        Err("the process ended here".into())
                    } else {
                        Ok(())
                    }
                });
                assert!(crashed.is_err(), "{cut:?}");
                assert!(
                    !shown(root),
                    "no note before the set-aside is done: {cut:?}"
                );
                let reserved = marker_at(root).unwrap().and_then(|marker| marker.revision);
                let finished = set_aside_at(root, || true, &mut |_| Ok(())).unwrap();
                let finished = finished.expect("the interrupted recovery finishes");
                if let Some(reserved) = reserved {
                    assert_eq!(finished.stop_target, reserved, "{cut:?}");
                }
                assert_eq!(
                    progress_file(root).unwrap().unwrap().revision,
                    finished.stop_target,
                    "the returned Stop target is already published: {cut:?}"
                );
                assert_eq!(
                    outcome(root),
                    expected,
                    "cut before {cut:?}, stop {stopped}"
                );
                assert_eq!(set_aside_at(root, || true, &mut |_| Ok(())).unwrap(), None);
            }
        }
    }
}
