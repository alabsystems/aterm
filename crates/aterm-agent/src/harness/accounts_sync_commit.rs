// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Crash recovery for account state, sync, renewal and pool materialization. The pending receipt fences readers;
//! replay installs exact admitted images, including their already allocated generations.
//! This is not atomicity for unrelated transactions or independent unlocked paired reads.

use super::*;
use serde::{Deserialize, Serialize};

type NativePublicationResult =
    Result<Option<super::super::codex_grant_owner::OwnerWitnessTransition>, String>;
type NoNativePublication = for<'a> fn(ActualPublicationPermit<'a>) -> NativePublicationResult;

const RECEIPT: &str = "sync-commit.json";
const WITNESS: &str = "sync-commit.seen";
const MAX_RECEIPT: u64 = 32 * 1024 * 1024;
const MAX_LEDGER: u64 = 8 * LEDGER_MAX_BYTES;
const MAX_FLEET: u64 = 4 * 1024 * 1024;

/// Caller authority is not recovered from a receipt's serialized path.
#[derive(Clone, Copy, Default)]
struct Scope<'a> {
    roster: Option<&'a Path>,
    fleet: Option<&'a Path>,
}

pub(in crate::harness) struct SyncTarget<'a> {
    pub path: &'a Path,
    pub before: Option<&'a str>,
    pub after: Option<&'a str>,
    pub mode: Option<u32>,
    /// The logical roster and local fleet image, never an arbitrary extra destination.
    pub fleet: Option<(&'a Path, &'a str)>,
}

enum WakeRule {
    Always,
    Events,
}

struct Publication<'a> {
    target: Option<SyncTarget<'a>>,
    wake: WakeRule,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Image {
    before: Option<String>,
    after: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FleetImage {
    path: String,
    mode: Option<u32>,
    image: Image,
}

enum StateTarget<'a> {
    Fleet(&'a Path, &'a str),
    Roster(SyncTarget<'a>),
}

struct PreparedStateTarget {
    fleet: Option<(PathBuf, FleetImage)>,
    roster_path: PathBuf,
    roster_name: String,
    roster_mode: Option<u32>,
    roster: Image,
    logins: Image,
}

impl Image {
    fn check(&self, max: u64) -> Result<(), String> {
        if self
            .before
            .iter()
            .chain(self.after.iter())
            .any(|s| s.len() as u64 > max)
            || (self.before.is_some() && self.after.is_none())
        {
            return Err("invalid sync image size or removal".into());
        }
        Ok(())
    }

    fn admits(&self, current: &Option<String>) -> bool {
        current == &self.before || current == &self.after
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    // A missing path admits only fixed paths below this Store; local_logins adds bindings.
    // Some serializes identically to the original string field, preserving old digests.
    roster_path: Option<String>,
    roster_mode: Option<u32>,
    // Omitted in existing receipts so their digest preimages remain unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    local_logins: bool,
    state: Image,
    logins: Image,
    ledger: Image,
    roster: Image,
    // Old receipts omit this field and keep their unconditional notification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wake: Option<bool>,
    // Omission preserves the canonical encoding and digest of admitted older receipts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fleet: Option<FleetImage>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u8,
    id: String,
    plan: Option<Plan>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredReceipt {
    receipt: Receipt,
    digest: String,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn receipt_digest(receipt: &Receipt) -> Result<String, String> {
    let encoded = aterm_json::to_string(receipt).map_err(|why| why.to_string())?;
    Ok(aterm_codec::hex::encode_lower(
        &aterm_digest::Sha256::digest(encoded.as_bytes()),
    ))
}

fn encode_stored_receipt(receipt: &Receipt) -> Result<String, String> {
    let stored = StoredReceipt {
        receipt: receipt.clone(),
        digest: receipt_digest(receipt)?,
    };
    let text = aterm_json::to_string(&stored).map_err(|why| why.to_string())?;
    if text.len() as u64 > MAX_RECEIPT {
        return Err("account sync receipt exceeds its limit".into());
    }
    Ok(text)
}

fn read(path: &Path, max: u64) -> Result<Option<String>, String> {
    use std::io::Read as _;
    let file = match astream_sys::open_nonblocking(path, astream_sys::Links::Refuse) {
        Ok(file) => file,
        Err(why) if why.kind() == io::ErrorKind::NotFound => {
            return match std::fs::symlink_metadata(path) {
                Err(why) if why.kind() == io::ErrorKind::NotFound => Ok(None),
                _ => Err(format!("{}: missing or linked sync file", path.display())),
            };
        }
        Err(why) => return Err(format!("{}: {why}", path.display())),
    };
    let meta = file.metadata().map_err(|why| why.to_string())?;
    if !meta.is_file() {
        return Err(format!("{}: sync file is not regular", path.display()));
    }
    if meta.len() > max {
        return Err(format!("{}: sync file exceeds its limit", path.display()));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0).saturating_add(1));
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|why| why.to_string())?;
    if bytes.len() as u64 > max {
        return Err(format!("{}: sync file exceeds its limit", path.display()));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|why| why.to_string())
}

fn absolute(path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path).map_err(|why| why.to_string())
}

pub(super) fn usage_target(path: Option<&Path>) -> Result<Option<PathBuf>, String> {
    path.map(|path| {
        let (path, _) = super::super::cli::roster_target(path)?;
        let path = absolute(&path)?;
        let mut ancestor = path.as_path();
        let mut missing = Vec::new();
        loop {
            match std::fs::canonicalize(ancestor) {
                Ok(mut resolved) => {
                    for name in missing.into_iter().rev() {
                        resolved.push(name);
                    }
                    return Ok(resolved);
                }
                Err(why) if why.kind() == io::ErrorKind::NotFound => {
                    if std::fs::symlink_metadata(ancestor).is_ok() {
                        return Err(format!("{}: unresolved usage source", ancestor.display()));
                    }
                    missing.push(
                        ancestor
                            .file_name()
                            .ok_or("usage source has no existing ancestor")?
                            .to_os_string(),
                    );
                    ancestor = ancestor
                        .parent()
                        .ok_or("usage source has no existing ancestor")?;
                }
                Err(why) => return Err(format!("{}: {why}", ancestor.display())),
            }
        }
    })
    .transpose()
}

/// Flush every directory on the way from the root to each of `dirs`, each one once: nothing here
/// changes an entry between two flushes of it, and an ancestor already flushed had its own
/// ancestors flushed after it, so the walk up from a later directory stops there.
fn durable_ancestry<'a>(dirs: impl IntoIterator<Item = &'a Path>) -> Result<(), String> {
    let mut flushed = BTreeSet::new();
    for dir in dirs {
        for ancestor in absolute(dir)?.ancestors() {
            if !flushed.insert(ancestor.to_path_buf()) {
                break;
            }
            aterm_durable::durable_dir(ancestor)
                .map_err(|why| format!("{}: {why}", ancestor.display()))?;
        }
    }
    Ok(())
}

/// Before creating a file, and for a retained absence, persist the closest existing
/// ancestry without creating configuration directories before the pending receipt.
fn publication_parent(path: &Path) -> &Path {
    let parent = path.parent().unwrap_or(Path::new("."));
    parent
        .ancestors()
        .find(|ancestor| ancestor.is_dir())
        .unwrap_or(parent)
}

impl Plan {
    fn paths(&self, store: &Store, scope: Scope<'_>) -> Result<Vec<(usize, PathBuf)>, String> {
        let mut paths = vec![(0, store.state_file()), (2, store.ledger_path())];
        if let Some(admitted) = &self.roster_path {
            if self.local_logins {
                return Err("local login receipt cannot name an external roster".into());
            }
            let roster = usage_target(Some(
                scope
                    .roster
                    .ok_or("account recovery needs its admitted roster path")?,
            ))?
            .ok_or("account recovery lost its roster path")?;
            // Re-resolve even an identical spelling: a formerly real parent can now
            // be a link. An old ambiguous lexical receipt stays retained and refused.
            if roster.to_str() != Some(admitted.as_str()) {
                return Err("account sync recovery target differs from the admitted roster".into());
            }
            paths.insert(1, (1, store.logins_file()));
            paths.push((3, roster));
        } else {
            if self.roster_mode.is_some()
                || self.roster.before.is_some()
                || self.roster.after.is_some()
            {
                return Err("local account receipt contains external roster images".into());
            }
            if self.local_logins {
                paths.insert(1, (1, store.logins_file()));
            } else if self.logins.before.is_some() || self.logins.after.is_some() {
                return Err("state-only account receipt contains login images".into());
            }
        }
        if let Some(fleet) = &self.fleet {
            let target = scope
                .fleet
                .ok_or("account recovery needs its caller's fleet scope")?;
            if target.to_str() != Some(fleet.path.as_str()) {
                return Err("account sync recovery target differs from the admitted fleet".into());
            }
            paths.push((4, target.to_path_buf()));
        }
        Ok(paths)
    }

    fn validate(&self, store: &Store, scope: Scope<'_>) -> Result<(), String> {
        if self.roster_mode.is_some_and(|mode| mode & !0o777 != 0) {
            return Err("account sync receipt has invalid roster permissions".into());
        }
        let mut names = BTreeSet::new();
        for (_, path) in self.paths(store, scope)? {
            let name = usage_target(Some(&path))?.ok_or("account sync path is missing")?;
            if !names.insert(name) {
                return Err("account sync files must have distinct destinations".into());
            }
        }
        if let Some(fleet) = &self.fleet {
            if fleet.mode.is_some_and(|mode| mode & !0o777 != 0) {
                return Err("account sync receipt has invalid fleet permissions".into());
            }
            fleet.image.check(MAX_FLEET)?;
            if let Some(text) = &fleet.image.after {
                super::super::accounts_sync::FleetRoster::from_toml_for(
                    store.cli,
                    text,
                    &fleet.path,
                )?;
            }
            validate_fleet_target(store, scope.fleet.ok_or("missing fleet scope")?)?;
        }
        for (image, max) in [
            (&self.state, MAX_STATE_BYTES),
            (&self.logins, MAX_LOGINS_BYTES),
            (&self.ledger, MAX_LEDGER),
            (&self.roster, MAX_ROSTER_BYTES),
        ] {
            image.check(max)?;
        }
        if let Some(text) = &self.state.after {
            State::from_json(text, 0).map_err(|why| why.why().to_owned())?;
        }
        if let Some(text) = &self.logins.after {
            Logins::from_json(text, 0).map_err(|why| why.why().to_owned())?;
        }
        if self.roster_path.is_some()
            && let Some(text) = self.roster.after.as_deref()
        {
            Roster::parse_for(store.cli, text)?;
        }
        Ok(())
    }

    fn images(&self) -> impl Iterator<Item = &Image> {
        [&self.state, &self.logins, &self.ledger, &self.roster]
            .into_iter()
            .chain(self.fleet.iter().map(|fleet| &fleet.image))
    }
}

/// Resolve the local fleet from the logical configuration location, before resolving the
/// roster itself. Those siblings can live in different directories when a roster is linked.
fn fleet_target(roster: &Path) -> Result<(PathBuf, Option<u32>), String> {
    let path = roster.with_file_name(super::super::accounts_sync::FLEET_ROSTER_FILE);
    let (target, mode) = super::super::cli::roster_target(&path)?;
    let target = usage_target(Some(&target))?.ok_or("fleet destination is missing")?;
    if usage_target(Some(roster))?.as_deref() == Some(target.as_path()) {
        return Err("fleet destination aliases its account roster".into());
    }
    Ok((target, mode))
}

fn prepare_fleet_image(
    store: &Store,
    roster: &Path,
    text: &str,
) -> Result<(PathBuf, FleetImage), String> {
    let (path, mode) = fleet_target(roster)?;
    validate_fleet_target(store, &path)?;
    super::super::accounts_sync::FleetRoster::from_toml_for(store.cli, text, "local fleet")?;
    let image = FleetImage {
        path: path.to_str().ok_or("fleet path is not UTF-8")?.to_owned(),
        mode,
        image: Image {
            before: read(&path, MAX_FLEET)?,
            after: Some(text.to_owned()),
        },
    };
    image.image.check(MAX_FLEET)?;
    Ok((path, image))
}

fn validate_fleet_target(store: &Store, target: &Path) -> Result<(), String> {
    // A link must not grant authority over receipt, witness or lock files that are not
    // themselves images in this transaction. Check this before preparing any state too.
    for path in [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.state_witness_file(),
        store.logins_witness_file(),
        store.wake_path(),
        store.machine_file(),
        store.dir.join(RECEIPT),
        store.dir.join(WITNESS),
        store.dir.join(LOCK_FILE),
    ] {
        if usage_target(Some(&path))?.as_deref() == Some(target) {
            return Err("fleet destination aliases account authority".into());
        }
    }
    Ok(())
}

impl Store {
    /// Receipt bookkeeping is tested by crash recovery separately from retained
    /// account and credential images in provider admission projections.
    #[cfg(test)]
    pub(in crate::harness) fn is_sync_receipt_file(&self, path: &Path) -> bool {
        path == self.dir.join(RECEIPT) || path == self.dir.join(WITNESS)
    }

    /// State and history share the existing journal without requiring a roster or login file.
    /// A roster-bound interrupted write still requires its caller-supplied recovery target.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountStateRead",
            action = "Torn",
            project = "aterm_agent::harness::cli::tests::state_read::state_read_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountStateRetention",
            action = "Edit",
            project = "aterm_agent::harness::cli::tests::retention::retention_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountStateRetention",
            action = "EventOnly",
            project = "aterm_agent::harness::cli::tests::retention::retention_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountStateCommit",
            action = "Write",
            project = "aterm_agent::harness::accounts::sync_commit::state_commit_tests::state_commit_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountStateCommit",
            action = "Retry",
            project = "aterm_agent::harness::accounts::sync_commit::state_commit_tests::state_commit_transition"
        )
    )]
    pub(super) fn transact_state_held(
        &self,
        held: &Lock,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&mut State) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        self.transact_state_images_held(held, now, note, None, Some(edit))
            .map(|(events, _)| events)
    }

    /// Commit the local fleet projection with account state under one retained receipt.
    /// The caller supplies the logical roster scope, never an extra write destination.
    /// Returns whether any admitted image or event changed.
    pub(in crate::harness) fn transact_fleet_state_held(
        &self,
        held: &Lock,
        now: i64,
        note: &mut Option<String>,
        fleet: (&Path, &str),
        edit: impl FnOnce(&mut State) -> Result<Vec<Event>, String>,
    ) -> Result<bool, String> {
        self.transact_state_images_held(
            held,
            now,
            note,
            Some(StateTarget::Fleet(fleet.0, fleet.1)),
            Some(edit),
        )
        .map(|(_, changed)| changed)
    }

    /// Publish a provider roster and local pool state while preserving login records
    /// exactly. Adding metadata rows does not migrate or reauthenticate existing logins.
    pub(in crate::harness) fn transact_roster_state_held(
        &self,
        held: &Lock,
        now: i64,
        note: &mut Option<String>,
        target: SyncTarget<'_>,
        edit: impl FnOnce(&mut State) -> Result<Vec<Event>, String>,
    ) -> Result<bool, String> {
        self.transact_state_images_held(
            held,
            now,
            note,
            Some(StateTarget::Roster(target)),
            Some(edit),
        )
        .map(|(_, changed)| changed)
    }

    /// Roster-only publication retains the exact state without creating a machine
    /// identity or running a state edit. Login records remain read-only as well.
    pub(in crate::harness) fn transact_roster_held(
        &self,
        held: &Lock,
        now: i64,
        note: &mut Option<String>,
        target: SyncTarget<'_>,
    ) -> Result<bool, String> {
        self.transact_state_images_held(
            held,
            now,
            note,
            Some(StateTarget::Roster(target)),
            None::<fn(&mut State) -> Result<Vec<Event>, String>>,
        )
        .map(|(_, changed)| changed)
    }

    fn transact_state_images_held(
        &self,
        held: &Lock,
        now: i64,
        note: &mut Option<String>,
        target: Option<StateTarget<'_>>,
        edit: Option<impl FnOnce(&mut State) -> Result<Vec<Event>, String>>,
    ) -> Result<(Vec<Event>, bool), String> {
        // A scoped caller planned outside this lock. A newly pending commit invalidates that
        // plan; it must recover and plan again, not silently continue from stale inputs.
        if target.is_some() {
            self.sync_read_receipt()?;
        } else {
            self.recover_sync_at(held, None, &mut |_| Ok(()))?;
        }
        let target = target
            .map(|target| {
                let (roster_path, roster_mode, roster, fleet) = match target {
                    StateTarget::Fleet(roster, text) => {
                        let fleet = prepare_fleet_image(self, roster, text)?;
                        let path = usage_target(Some(roster))?.ok_or("missing roster target")?;
                        let before = read(&path, MAX_ROSTER_BYTES)?;
                        (
                            path,
                            None,
                            Image {
                                after: before.clone(),
                                before,
                            },
                            Some(fleet),
                        )
                    }
                    StateTarget::Roster(target) => {
                        if target.fleet.is_some() {
                            return Err(
                                "provider roster-state publication cannot carry a fleet target"
                                    .to_owned(),
                            );
                        }
                        let path =
                            usage_target(Some(target.path))?.ok_or("missing roster target")?;
                        (
                            path,
                            target.mode,
                            Image {
                                before: target.before.map(str::to_owned),
                                after: target.after.map(str::to_owned),
                            },
                            None,
                        )
                    }
                };
                let roster_name = roster_path
                    .to_str()
                    .ok_or("roster path is not UTF-8")?
                    .to_owned();
                for text in roster.before.iter().chain(roster.after.iter()) {
                    Roster::parse_for(self.cli, text)?;
                }
                let before = read(&self.logins_file(), MAX_LOGINS_BYTES)?;
                if let Some(text) = &before {
                    Logins::from_json(text, now).map_err(|why| why.why().to_owned())?;
                } else {
                    self.absent_logins()?;
                }
                let logins = Image {
                    after: before.clone(),
                    before,
                };
                Ok::<_, String>(PreparedStateTarget {
                    fleet,
                    roster_path,
                    roster_name,
                    roster_mode,
                    roster,
                    logins,
                })
            })
            .transpose()?;
        let old_state = read(&self.state_file(), MAX_STATE_BYTES)?;
        let (events, new_state, notes, auth_changed) = if let Some(edit) = edit {
            let (mut state, read_note, machine_kept) = self.state_as_stored(now)?;
            *note = read_note;
            if !machine_kept && let Ok(id) = self.machine_id(&state.machine) {
                state.machine = id;
            }
            let before = state.clone();
            let mut events = edit(&mut state)?;
            let auth_changed = auth_scope::clear(&mut state);
            let new_state = if state != before || (!events.is_empty() && old_state.is_none()) {
                drop_stale_sessions(&mut state, now);
                Some(state.to_json() + "\n")
            } else {
                old_state.clone()
            };
            // Attribute ledger rows before retaining the transaction's exact images.
            let notes = super::super::journal::observe(self.cli, &before, &state, &mut events, now);
            (events, new_state, Some(notes), auth_changed)
        } else {
            if old_state.is_none() {
                self.absent_state()?;
            }
            (Vec::new(), old_state.clone(), None, false)
        };
        let plan = Plan {
            local_logins: false,
            roster_path: target.as_ref().map(|target| target.roster_name.clone()),
            roster_mode: target.as_ref().and_then(|target| target.roster_mode),
            state: Image {
                before: old_state,
                after: new_state,
            },
            logins: target
                .as_ref()
                .map(|target| target.logins.clone())
                .unwrap_or_default(),
            ledger: self.ledger_image(&events)?,
            roster: target
                .as_ref()
                .map(|target| target.roster.clone())
                .unwrap_or_default(),
            wake: Some(
                auth_changed
                    || events.iter().any(Event::wakes)
                    || target
                        .as_ref()
                        .is_some_and(|target| target.roster.before != target.roster.after),
            ),
            fleet: target
                .as_ref()
                .and_then(|target| target.fleet.as_ref().map(|(_, image)| image.clone())),
        };
        let changed = plan.images().any(|image| image.before != image.after) || !events.is_empty();
        self.publish_plan_scoped(
            held,
            Scope {
                roster: target.as_ref().map(|target| target.roster_path.as_path()),
                fleet: target
                    .as_ref()
                    .and_then(|target| target.fleet.as_ref().map(|(path, _)| path.as_path())),
            },
            plan,
            &events,
            || Ok(()),
        )?;
        if let Some(notes) = notes {
            super::super::journal::publish(&notes);
        }
        Ok((events, changed))
    }

    /// Publish local bindings, state and history without inventing a roster target.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountLocalCommit",
            action = "Commit",
            project = "aterm_agent::harness::accounts::sync_commit::local_commit_tests::local_transition"
        )
    )]
    pub(super) fn transact_local_logins_held(
        &self,
        held: &Lock,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&mut State, &mut Logins) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        self.recover_sync_at(held, None, &mut |_| Ok(()))?;
        self.transact_images_held(
            held,
            now,
            note,
            Publication {
                target: None,
                wake: WakeRule::Events,
            },
            edit,
            || Ok(()),
        )
    }

    /// Publish account records with the current roster as an unchanged admission image.
    /// A previous admitted commit completes before this edit sees its input snapshots.
    pub(in crate::harness) fn transact_roster_logins_within(
        &self,
        ladder: &[std::time::Duration],
        roster: &Path,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&Roster, &mut State, &mut Logins) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        let held = self
            .acquire_within(ladder)
            .map_err(|why| why.to_string())?
            .ok_or_else(|| LOCK_HELD.to_owned())?;
        self.transact_roster_logins_held(&held, roster, now, note, edit)
    }

    /// The roster-bound transaction while the caller retains the account writer lock.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountRenewalCommit",
            action = "Commit",
            project = "aterm_agent::harness::accounts::renewal_commit_tests::commit_transition"
        )
    )]
    pub(in crate::harness) fn transact_roster_logins_held(
        &self,
        held: &Lock,
        roster: &Path,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&Roster, &mut State, &mut Logins) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        self.transact_roster_records_held(held, roster, now, note, edit, WakeRule::Always)
    }

    /// Pool observations preserve the legacy event-based wake rule while committing durably.
    pub(in crate::harness) fn transact_pool_logins_within(
        &self,
        ladder: &[std::time::Duration],
        roster: &Path,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&Roster, &mut State, &mut Logins) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        let held = self
            .acquire_within(ladder)
            .map_err(|why| why.to_string())?
            .ok_or_else(|| LOCK_HELD.to_owned())?;
        self.transact_pool_logins_held(&held, roster, now, note, edit)
    }

    /// Exercise an admitted observation through the real roster-bound writer transaction.
    ///
    /// # Errors
    /// Returns an error when the roster, admission or durable transaction refuses.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn observe_admitted_workflow_for_test(
        &self,
        roster: &Path,
        label: &str,
        now: i64,
        observe: impl FnOnce(&Roster, &mut State, &mut Logins) -> Vec<Event>,
    ) -> Result<(), String> {
        self.transact_pool_logins_within(&[], roster, now, &mut None, |raw, state, logins| {
            workflow_scope::admit(logins, label)?;
            Ok(observe(raw, state, logins))
        })
        .map(|_| ())
    }

    /// The pool transaction under its caller's existing account writer lock.
    pub(in crate::harness) fn transact_pool_logins_held(
        &self,
        held: &Lock,
        roster: &Path,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&Roster, &mut State, &mut Logins) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        self.transact_roster_records_held(held, roster, now, note, edit, WakeRule::Events)
    }

    /// Test convenience for owning a writer before the actual native callback.
    /// Production callers retain their writer before opening owner/Auth claims.
    #[cfg(test)]
    pub(in crate::harness) fn transact_roster_logins_native_within(
        &self,
        ladder: &[std::time::Duration],
        roster: &Path,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&Roster, &mut State, &mut Logins) -> Result<Vec<Event>, String>,
        native_publication: impl FnOnce(ActualPublicationPermit<'_>) -> NativePublicationResult,
    ) -> Result<Vec<Event>, String> {
        let held = self
            .acquire_within(ladder)
            .map_err(|why| why.to_string())?
            .ok_or_else(|| LOCK_HELD.to_owned())?;
        self.transact_roster_logins_native_held(&held, roster, now, note, edit, native_publication)
    }

    /// Native publication under the caller's already-held canonical writer;
    /// its actual generation callback and durability path are unchanged.
    pub(in crate::harness) fn transact_roster_logins_native_held(
        &self,
        held: &Lock,
        roster: &Path,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&Roster, &mut State, &mut Logins) -> Result<Vec<Event>, String>,
        native_publication: impl FnOnce(ActualPublicationPermit<'_>) -> NativePublicationResult,
    ) -> Result<Vec<Event>, String> {
        self.transact_roster_records_native_held(
            held,
            roster,
            now,
            note,
            edit,
            WakeRule::Always,
            Some(native_publication),
        )
    }

    fn transact_roster_records_held(
        &self,
        held: &Lock,
        roster: &Path,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&Roster, &mut State, &mut Logins) -> Result<Vec<Event>, String>,
        wake: WakeRule,
    ) -> Result<Vec<Event>, String> {
        self.transact_roster_records_native_held(
            held,
            roster,
            now,
            note,
            edit,
            wake,
            None::<NoNativePublication>,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "existing held transaction plus its optional native publication callback"
    )]
    fn transact_roster_records_native_held(
        &self,
        held: &Lock,
        roster: &Path,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&Roster, &mut State, &mut Logins) -> Result<Vec<Event>, String>,
        wake: WakeRule,
        native_publication: Option<
            impl FnOnce(ActualPublicationPermit<'_>) -> NativePublicationResult,
        >,
    ) -> Result<Vec<Event>, String> {
        self.recover_sync_held(held, roster)?;
        let (target, mode) = super::super::cli::roster_target(roster)?;
        let text = read(&target, MAX_ROSTER_BYTES)?
            .ok_or_else(|| format!("{}: account roster is missing", roster.display()))?;
        let raw = Roster::parse_for(self.cli, &text)?;
        let cli = self.cli;
        self.transact_images_native_held(
            held,
            now,
            note,
            Publication {
                target: Some(SyncTarget {
                    fleet: None,
                    path: &target,
                    before: Some(&text),
                    after: Some(&text),
                    mode,
                }),
                wake,
            },
            |state, logins| {
                let events = edit(&raw, state, logins)?;
                super::super::journal::learn(cli, &raw, state);
                Ok(events)
            },
            || Ok(()),
            native_publication,
        )
    }

    /// Read an observed usage scope while holding its account writer, without publishing
    /// account state. Recovery completes before the reader can encounter a pending receipt.
    /// # Errors
    /// The store or target differs, the writer cannot be acquired, recovery or reading
    /// fails, or the caller's inspection rejects the snapshot.
    pub(in crate::harness) fn inspect_usage_within<T>(
        &self,
        ladder: &[std::time::Duration],
        captured: &UsageRoster,
        now: i64,
        inspect: impl FnOnce(&AccountRecords) -> Result<T, String>,
    ) -> Result<T, String> {
        if self.dir != captured.store.dir || self.cli != captured.store.cli {
            return Err("usage observation belongs to another account store".into());
        }
        let held = self
            .acquire_within(ladder)
            .map_err(|why| why.to_string())?
            .ok_or_else(|| LOCK_HELD.to_owned())?;
        let target = usage_target(captured.path.as_deref())?;
        if target != captured.target {
            return Err("usage roster target changed; nothing recorded".into());
        }
        self.recover_sync_at(&held, captured.path.as_deref(), &mut |_| Ok(()))?;
        let records = self
            .read_account_records(captured.path.as_deref(), now)
            .map_err(|why| match why {
                super::RosterReadError::Roster(why) | super::RosterReadError::Records(why) => why,
            })?;
        if usage_target(captured.path.as_deref())? != target {
            return Err("usage roster target changed during read; nothing recorded".into());
        }
        inspect(&records)
    }

    /// Usage also works without configured rows. An absent file is a retained absence
    /// in the receipt, so a later configuration cannot silently admit old observations.
    /// `seen` reads the state once the edit ran and the journal learned its rows' keys
    /// ([`super::super::journal::learn`]), so what it reads carries the keys this transaction
    /// learns; it runs only where the edit succeeded.
    pub(super) fn transact_usage_within<T>(
        &self,
        ladder: &[std::time::Duration],
        captured: &UsageRoster,
        now: i64,
        edit: impl FnOnce(&Roster, &mut State, &mut Logins) -> Result<Vec<Event>, String>,
        seen: impl FnOnce(&State) -> T,
    ) -> Result<(Vec<Event>, T), String> {
        let held = self
            .acquire_within(ladder)
            .map_err(|why| format!("{}: {why}", self.dir.display()))?
            .ok_or_else(|| LOCK_HELD.to_owned())?;
        let target = usage_target(captured.path.as_deref())?;
        if target != captured.target {
            return Err("usage roster target changed; nothing recorded".into());
        }
        self.recover_sync_at(&held, captured.path.as_deref(), &mut |_| Ok(()))?;
        let text = target
            .as_ref()
            .map(|path| read(path, MAX_ROSTER_BYTES))
            .transpose()?
            .flatten();
        let raw = text
            .as_deref()
            .map(|text| Roster::parse_for(self.cli, text))
            .transpose()?
            .unwrap_or_default();
        let cli = self.cli;
        let mut read_back = None;
        let events = self.transact_images_held(
            &held,
            now,
            &mut None,
            Publication {
                target: target.as_ref().map(|path| SyncTarget {
                    fleet: None,
                    path,
                    before: text.as_deref(),
                    after: text.as_deref(),
                    mode: None,
                }),
                wake: WakeRule::Events,
            },
            |state, logins| {
                let events = edit(&raw, state, logins)?;
                super::super::journal::learn(cli, &raw, state);
                read_back = Some(seen(state));
                Ok(events)
            },
            || Ok(()),
        )?;
        let read_back = read_back.ok_or("usage transaction committed without its edit")?;
        Ok((events, read_back))
    }

    pub(in crate::harness) fn sync_needs_recovery(&self) -> bool {
        self.receipt()
            .map_or(true, |receipt| receipt.is_some_and(|r| r.plan.is_some()))
    }

    pub(super) fn sync_writer_barrier(&self) -> io::Result<()> {
        if self.receipt().map_err(io::Error::other)?.is_some() {
            // A completed receipt may be visible after an uncertain rename barrier.
            // Persist it before any later writer can change the admitted images.
            aterm_durable::durable_dir(&self.dir)?;
        }
        Ok(())
    }

    fn receipt(&self) -> Result<Option<Receipt>, String> {
        let Some(text) = read(&self.dir.join(RECEIPT), MAX_RECEIPT)? else {
            return match std::fs::symlink_metadata(self.dir.join(WITNESS)) {
                Err(why) if why.kind() == io::ErrorKind::NotFound => Ok(None),
                _ => Err(
                    "account sync receipt is missing; retained witness prevents a fresh start"
                        .into(),
                ),
            };
        };
        let stored: StoredReceipt = aterm_json::from_str(&text)
            .map_err(|why| format!("invalid account sync receipt: {why}"))?;
        if receipt_digest(&stored.receipt)? != stored.digest {
            return Err("account sync receipt digest differs; no files were recovered".into());
        }
        let receipt = stored.receipt;
        if receipt.schema != 1
            || receipt.id.len() != 32
            || !aterm_codec::hex::is_hex(&receipt.id, 32)
        {
            return Err("invalid account sync receipt identity or schema".into());
        }
        Ok(Some(receipt))
    }

    pub(in crate::harness) fn sync_read_receipt(&self) -> Result<Option<String>, String> {
        let receipt = self.receipt()?;
        if receipt.as_ref().is_some_and(|r| r.plan.is_some()) {
            return Err(format!(
                "interrupted account sync; run `{}` to resume before using accounts",
                crate::harness::accounts_step::Step::Sync(self.cli)
            ));
        }
        Ok(receipt.map(|r| r.id))
    }

    pub(in crate::harness) fn check_sync_read_receipt(
        &self,
        before: &Option<String>,
    ) -> Result<(), String> {
        if &self.sync_read_receipt()? != before {
            return Err("account sync changed during this read; read again".into());
        }
        Ok(())
    }

    fn save_receipt(&self, receipt: &Receipt) -> Result<(), String> {
        let text = encode_stored_receipt(receipt)?;
        self.ensure().map_err(|why| why.to_string())?;
        super::super::upgrade_models::write_atomic_mode(&self.dir.join(RECEIPT), &text, Some(0o600))
            .and_then(|()| aterm_durable::durable_dir(&self.dir))
            .map_err(|why| format!("account sync receipt: {why}"))
    }

    /// Resume state-only images at fixed store paths, never a receipt-supplied path.
    /// All images must still be admitted before any replacement is attempted.
    pub(super) fn recover_state_within(
        &self,
        ladder: &[std::time::Duration],
    ) -> Result<bool, String> {
        self.recover_within(ladder, None, || {})
    }

    /// Resume a publication at the caller's roster, or state-only fixed store paths.
    pub(in crate::harness) fn recover_sync_within(
        &self,
        ladder: &[std::time::Duration],
        roster: &Path,
    ) -> Result<bool, String> {
        self.recover_within(ladder, Some(roster), || {})
    }

    fn recover_within(
        &self,
        ladder: &[std::time::Duration],
        roster: Option<&Path>,
        after_peek: impl FnOnce(),
    ) -> Result<bool, String> {
        if self.receipt()?.is_none_or(|receipt| receipt.plan.is_none()) {
            return Ok(false);
        }
        after_peek();
        let held = self
            .acquire_within(ladder)
            .map_err(|why| why.to_string())?
            .ok_or_else(|| LOCK_HELD.to_owned())?;
        // Another writer may have completed the peeked plan or admitted a new scope.
        let Some(plan) = self.receipt()?.and_then(|receipt| receipt.plan) else {
            return Ok(false);
        };
        if let Some(roster) = roster.filter(|_| plan.roster_path.is_some() || plan.fleet.is_some())
        {
            let (target, _) = super::super::cli::roster_target(roster)?;
            let fleet = plan
                .fleet
                .as_ref()
                .map(|_| fleet_target(roster))
                .transpose()?;
            self.recover_sync_scoped_at(
                &held,
                Scope {
                    roster: plan.roster_path.as_ref().map(|_| target.as_path()),
                    fleet: fleet.as_ref().map(|(path, _)| path.as_path()),
                },
                &mut |_| Ok(()),
            )
        } else {
            self.recover_sync_at(&held, None, &mut |_| Ok(()))
        }
    }

    /// Recover while the caller retains the writer lock. `roster` is the caller's logical
    /// configuration path; resolving it first loses authority for a sibling fleet file.
    pub(in crate::harness) fn recover_sync_held(
        &self,
        held: &Lock,
        roster: &Path,
    ) -> Result<bool, String> {
        self.recover_sync_with(held, roster, &mut |_| Ok(()))
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSyncCommit",
            action = "Recover",
            project = "aterm_agent::harness::accounts::sync_commit::tests::sync_transition"
        )
    )]
    fn recover_sync_with(
        &self,
        held: &Lock,
        roster: &Path,
        progress: &mut dyn FnMut(usize) -> Result<(), String>,
    ) -> Result<bool, String> {
        self.recover_sync_at(held, Some(roster), progress)
    }

    fn recover_sync_at(
        &self,
        held: &Lock,
        roster: Option<&Path>,
        progress: &mut dyn FnMut(usize) -> Result<(), String>,
    ) -> Result<bool, String> {
        let fleet = if self
            .receipt()?
            .is_some_and(|receipt| receipt.plan.is_some_and(|plan| plan.fleet.is_some()))
        {
            Some(fleet_target(
                roster.ok_or("account recovery needs its caller's fleet scope")?,
            )?)
        } else {
            None
        };
        self.recover_sync_scoped_at(
            held,
            Scope {
                roster,
                fleet: fleet.as_ref().map(|(path, _)| path.as_path()),
            },
            progress,
        )
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountFleetCommit",
            action = "Recover",
            project = "aterm_agent::harness::accounts::sync_commit::fleet_commit_tests::fleet_transition"
        )
    )]
    fn recover_sync_scoped_at(
        &self,
        held: &Lock,
        scope: Scope<'_>,
        progress: &mut dyn FnMut(usize) -> Result<(), String>,
    ) -> Result<bool, String> {
        let Some(receipt) = self.receipt()? else {
            return Ok(false);
        };
        let Some(plan) = &receipt.plan else {
            return Ok(false);
        };
        plan.validate(self, scope)?;
        self.replay(held, receipt, scope, progress)?;
        Ok(true)
    }

    /// Install the admitted images of `receipt`'s validated plan and complete it.
    fn replay(
        &self,
        _held: &Lock,
        mut receipt: Receipt,
        scope: Scope<'_>,
        progress: &mut dyn FnMut(usize) -> Result<(), String>,
    ) -> Result<(), String> {
        let Some(plan) = receipt.plan.take() else {
            return Ok(());
        };
        let paths = plan.paths(self, scope)?;
        let images: Vec<_> = plan.images().collect();
        let limits = [
            MAX_STATE_BYTES,
            MAX_LOGINS_BYTES,
            MAX_LEDGER,
            MAX_ROSTER_BYTES,
            MAX_FLEET,
        ];
        for &(index, ref path) in &paths {
            let image = images[index];
            let max = limits[index];
            image.check(max)?;
            if !image.admits(&read(path, max)?) {
                return Err(format!(
                    "{} changed outside the interrupted sync; recovery preserved every file",
                    path.display()
                ));
            }
        }
        progress(0)?;
        for &(index, ref path) in &paths {
            let image = images[index];
            let max = limits[index];
            let current = read(path, max)?;
            if !image.admits(&current) {
                return Err(format!(
                    "{} changed during recovery; conflicting bytes were preserved",
                    path.display()
                ));
            }
            if index < 2 && image.after.is_some() {
                let witness = if index == 0 {
                    self.state_witness_file()
                } else {
                    self.logins_witness_file()
                };
                self.retain_witness(&witness)
                    .map_err(|why| why.to_string())?;
            }
            if current != image.after {
                let text = image.after.as_deref().ok_or("sync cannot remove files")?;
                let mode = match index {
                    3 => plan.roster_mode,
                    4 => plan.fleet.as_ref().and_then(|fleet| fleet.mode),
                    _ => Some(0o600),
                };
                super::super::upgrade_models::write_atomic_mode(path, text, mode)
                    .map_err(|why| format!("{}: {why}", path.display()))?;
            }
            // An earlier rename may have succeeded before its directory barrier failed.
            // Matching bytes still need that barrier before the receipt can complete.
            // A missing roster may also have missing parent directories. Preserve its
            // absence without creating configuration; persist the nearest existing parent.
            let barrier = publication_parent(path);
            if matches!(index, 3 | 4) {
                durable_ancestry(std::iter::once(barrier))?;
            } else {
                aterm_durable::durable_dir(barrier)
                    .map_err(|why| format!("{}: {why}", path.display()))?;
            }
            progress(index + 1)?;
        }
        if plan.wake.unwrap_or(true) {
            self.touch_wake().map_err(|why| why.to_string())?;
        }
        aterm_durable::durable_dir(&self.dir).map_err(|why| why.to_string())?;
        progress(if plan.fleet.is_some() { 6 } else { 5 })?;
        self.save_receipt(&receipt)
    }

    /// Prepare all authority and generations before auxiliary publication. Once admitted,
    /// a pending receipt owns the exact images until completion or a conflict refusal.
    pub(in crate::harness) fn transact_sync_held(
        &self,
        held: &Lock,
        now: i64,
        note: &mut Option<String>,
        target: SyncTarget<'_>,
        edit: impl FnOnce(&mut State, &mut Logins) -> Result<Vec<Event>, String>,
        auxiliary: impl FnOnce() -> Result<(), String>,
    ) -> Result<Vec<Event>, String> {
        // This path runs no recovery of its own: a pending or damaged receipt refuses it here.
        self.sync_read_receipt()?;
        self.transact_images_held(
            held,
            now,
            note,
            Publication {
                target: Some(target),
                wake: WakeRule::Always,
            },
            edit,
            auxiliary,
        )
    }

    /// The edit and publication of one transaction, whose caller has seen no pending receipt
    /// under the writer lock `held`: no receipt can change while it holds that lock, so the
    /// snapshots below read with no receipt check of their own.
    fn transact_images_held(
        &self,
        held: &Lock,
        now: i64,
        note: &mut Option<String>,
        publication: Publication<'_>,
        edit: impl FnOnce(&mut State, &mut Logins) -> Result<Vec<Event>, String>,
        auxiliary: impl FnOnce() -> Result<(), String>,
    ) -> Result<Vec<Event>, String> {
        self.transact_images_native_held(
            held,
            now,
            note,
            publication,
            edit,
            auxiliary,
            None::<NoNativePublication>,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "existing held transaction plus its optional native publication callback"
    )]
    fn transact_images_native_held(
        &self,
        held: &Lock,
        now: i64,
        note: &mut Option<String>,
        publication: Publication<'_>,
        edit: impl FnOnce(&mut State, &mut Logins) -> Result<Vec<Event>, String>,
        auxiliary: impl FnOnce() -> Result<(), String>,
        native_publication: Option<
            impl FnOnce(ActualPublicationPermit<'_>) -> NativePublicationResult,
        >,
    ) -> Result<Vec<Event>, String> {
        let Publication { target, wake } = publication;
        let roster = usage_target(target.as_ref().map(|target| target.path))?;
        let fleet = target
            .as_ref()
            .and_then(|target| target.fleet)
            .map(|(logical, text)| {
                if roster != usage_target(Some(logical))? {
                    return Err("local fleet scope names another account roster".to_owned());
                }
                prepare_fleet_image(self, logical, text)
            })
            .transpose()?;
        let (mut state, state_note, machine_kept) = self.state_as_stored(now)?;
        let (mut logins, login_note) = self.logins_as_stored(now)?;
        *note = state_note.or(login_note);
        if !machine_kept && let Ok(id) = self.machine_id(&state.machine) {
            state.machine = id;
        }
        let before = (state.clone(), logins.clone());
        let raw_before = target
            .as_ref()
            .map(|target| {
                target.before.map_or_else(
                    || Ok(Roster::default()),
                    |text| Roster::parse_for(self.cli, text),
                )
            })
            .transpose()?;
        let raw_after = target
            .as_ref()
            .map(|target| {
                target.after.map_or_else(
                    || Ok(Roster::default()),
                    |text| Roster::parse_for(self.cli, text),
                )
            })
            .transpose()?;
        if let Some(raw) = raw_before.as_ref() {
            self.hydrate_lapsed(&mut state, &bound(raw, &logins));
        }
        workflow_scope::begin(raw_before.as_ref(), &mut logins);
        let managed_before = raw_before.as_ref().filter(|_| !state.contextual_auth);
        auth_scope::begin(&mut state, managed_before, &mut logins);
        let mut events = edit(&mut state, &mut logins)?;
        let final_raw = if state.pending_auth.is_empty() {
            None
        } else {
            target
                .as_ref()
                .map(|target| {
                    target.after.map_or(Ok(Roster::default()), |text| {
                        Roster::parse_for(self.cli, text)
                    })
                })
                .transpose()?
        };
        events.extend(self.finalize_auth(&mut state, &mut logins, final_raw.as_ref())?);
        advance_renewal_generations(&before.1, &mut logins, &events)?;
        let managed_after = raw_after.as_ref().filter(|_| !state.contextual_auth);
        let auth_changed =
            auth_scope::finish(&mut state, &before.1, managed_after, &mut logins, &events)?;
        let workflow_changed =
            workflow_scope::finish(&before.1, raw_after.as_ref(), &mut logins, &mut events)?;
        let auth_changed = auth_changed || workflow_changed;
        advance_sign_in_receipts(&before.1, &mut logins, &state, &events)?;
        if let Some(native_publication) = native_publication {
            let derived = logins.clone();
            let channel = super::native_publication::PublicationChannel::new();
            let transition = native_publication(ActualPublicationPermit::new(
                self, &before.1, &derived, &channel,
            ))?;
            let mut native_changed = false;
            // Authenticate the derived row image before the normal canonical
            // owner-witness accessor can accept a new row's Applied record.
            if let Some(receipt) = channel.into_receipt() {
                logins
                    .enrollment
                    .attach_row_publication(receipt, &logins.labels)?;
                native_changed = true;
            }
            if let Some(transition) = transition {
                if !transition.is_fresh_activation() || transition.expected().is_some() {
                    return Err("native publication requires fresh activation evidence".into());
                }
                logins.enrollment.cas_owner_witness(&transition)?;
                native_changed = true;
            }
            if native_changed {
                logins.enrollment.commit(
                    &derived.enrollment,
                    &Default::default(),
                    &logins.labels,
                )?;
            }
        }
        logins.enrollment.require_complete_row_publications()?;
        let old_state = read(&self.state_file(), MAX_STATE_BYTES)?;
        let old_logins = read(&self.logins_file(), MAX_LOGINS_BYTES)?;
        let new_state = if state != before.0 || (!events.is_empty() && old_state.is_none()) {
            drop_stale_sessions(&mut state, now);
            Some(state.to_json() + "\n")
        } else {
            old_state.clone()
        };
        let notes = super::super::journal::observe(self.cli, &before.0, &state, &mut events, now);
        let new_logins = if logins != before.1 {
            Some(logins.to_json() + "\n")
        } else {
            old_logins.clone()
        };
        let plan = Plan {
            local_logins: target.is_none(),
            roster_path: roster
                .as_ref()
                .map(|path| {
                    path.to_str()
                        .map(str::to_owned)
                        .ok_or("roster path is not UTF-8")
                })
                .transpose()?,
            roster_mode: target.as_ref().and_then(|target| target.mode),
            state: Image {
                before: old_state,
                after: new_state,
            },
            logins: Image {
                before: old_logins,
                after: new_logins,
            },
            ledger: self.ledger_image(&events)?,
            roster: target
                .as_ref()
                .map(|target| Image {
                    before: target.before.map(str::to_owned),
                    after: target.after.map(str::to_owned),
                })
                .unwrap_or_default(),
            wake: match wake {
                WakeRule::Always => None,
                WakeRule::Events => Some(auth_changed || events.iter().any(Event::wakes)),
            },
            fleet: fleet.as_ref().map(|(_, image)| image.clone()),
        };
        self.publish_plan_scoped(
            held,
            Scope {
                roster: roster.as_deref(),
                fleet: fleet.as_ref().map(|(path, _)| path.as_path()),
            },
            plan,
            &events,
            auxiliary,
        )?;
        super::super::journal::publish(&notes);
        Ok(events)
    }

    fn ledger_image(&self, events: &[Event]) -> Result<Image, String> {
        let before = read(&self.ledger_path(), MAX_LEDGER)?;
        let after = if events.is_empty() {
            before.clone()
        } else {
            let mut text = before.clone().unwrap_or_default();
            for event in events {
                text.push_str(&event.to_json_line());
                text.push('\n');
            }
            if text.len() as u64 > LEDGER_MAX_BYTES {
                let rows: Vec<_> = text
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .collect();
                text = rows[rows.len().saturating_sub(LEDGER_KEEP_ROWS)..].join("\n") + "\n";
            }
            Some(text)
        };
        Ok(Image { before, after })
    }

    fn publish_plan_scoped(
        &self,
        held: &Lock,
        scope: Scope<'_>,
        plan: Plan,
        events: &[Event],
        auxiliary: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        plan.validate(self, scope)?;
        let changed = plan.images().any(|image| image.before != image.after);
        // Entropy and serialization failures are also admission failures, not partial commits.
        let receipt = Receipt {
            schema: 1,
            id: aterm_uds::rand::hex_token::<16>().map_err(|why| why.to_string())?,
            plan: Some(plan),
        };
        encode_stored_receipt(&receipt)?;
        auxiliary()?;
        #[cfg(test)]
        if EARLY_ROSTER.with(std::cell::Cell::take)
            && let Some(path) = scope.roster
            && let Some(plan) = &receipt.plan
            && let Some(text) = plan.roster.after.as_deref()
        {
            // The conformance test's production mutant publishes the real candidate
            // before its receipt. Its no-early-image law must reject this ordering.
            super::super::upgrade_models::write_atomic_mode(path, text, plan.roster_mode)
                .map_err(|why| why.to_string())?;
        }
        // A bounded ledger can retain identical bytes after appending an identical event.
        // That event still needs its receipt and notification, even without a changed image.
        if !changed && events.is_empty() {
            return Ok(());
        }
        // A durable clean receipt precedes the first retained witness: a crash between the
        // witness and pending intent still has a valid clean receipt and no changed authority.
        if self.receipt()?.is_none() {
            let clean = Receipt {
                schema: 1,
                id: "0".repeat(32),
                plan: None,
            };
            self.save_receipt(&clean)?;
        }
        durable_ancestry(
            std::iter::once(self.dir.as_path())
                .chain(scope.roster.map(publication_parent))
                .chain(scope.fleet.map(publication_parent)),
        )?;
        self.retain_witness(&self.dir.join(WITNESS))
            .map_err(|why| why.to_string())?;
        self.save_receipt(&receipt)?;
        #[cfg(test)]
        let cut = CUT.with(std::cell::Cell::take);
        #[cfg(not(test))]
        let cut: Option<usize> = None;
        // The plan was validated above and is the receipt just saved, under the lock nothing else
        // replaces a receipt without, so it is neither read back nor validated again.
        self.replay(held, receipt, scope, &mut |step| {
            if cut == Some(step) {
                Err("injected interruption".to_owned())
            } else {
                Ok(())
            }
        })
    }
}

#[cfg(test)]
thread_local! {
    /// A test's crash: the next commit on this thread stops before replay step `n`, its receipt
    /// pending, as a process killed there would leave it.
    pub(in crate::harness) static CUT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    pub(in crate::harness) static EARLY_ROSTER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSyncCommit",
        action = "AnotherCase",
        reason = "Bounded fixture selection; no production account transition."
    )
)]
#[cfg(any(test, debug_assertions))]
#[allow(dead_code)]
fn model_case_selection() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRenewalCommit",
        action = "AnotherProvider",
        reason = "Bounded provider fixture selection; no production account transition."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRenewalCommit",
        action = "AnotherCase",
        reason = "Bounded fault fixture selection; no production account transition."
    )
)]
#[cfg(any(test, debug_assertions))]
#[allow(dead_code)]
fn model_renewal_case_selection() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountStateCommit",
        action = "AnotherProvider",
        reason = "Select a native provider fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountStateCommit",
        action = "AnotherOperation",
        reason = "Select a native state observation or notification fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountStateCommit",
        action = "AnotherFault",
        reason = "Select a native publication failure fixture."
    )
)]
#[cfg(any(test, debug_assertions))]
#[allow(dead_code)]
fn model_state_case_selection() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountLocalCommit",
        action = "AnotherProvider",
        reason = "Select a native provider fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountLocalCommit",
        action = "AnotherOperation",
        reason = "Select a renewal or metadata observation fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountLocalCommit",
        action = "AnotherFault",
        reason = "Select a publication failure fixture."
    )
)]
#[cfg(any(test, debug_assertions))]
#[allow(dead_code)]
fn model_local_case_selection() {}

#[cfg(test)]
#[path = "accounts_sync_commit_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "accounts_state_commit_tests.rs"]
mod state_commit_tests;

#[cfg(test)]
#[path = "accounts_local_commit_tests.rs"]
mod local_commit_tests;

#[cfg(test)]
#[path = "accounts_fleet_commit_tests.rs"]
mod fleet_commit_tests;

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountFleetCommit",
        action = "Ordinary",
        reason = "The caller selects an empty-pool or ordinary roster publication."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountFleetCommit",
        action = "Cut",
        reason = "The test selects a durable interruption; no production account mutation."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountFleetCommit",
        action = "Fault",
        reason = "The environment supplies a conflicting edit or caller scope."
    )
)]
#[cfg(any(test, debug_assertions))]
#[allow(dead_code)]
fn model_fleet_case_selection() {}
