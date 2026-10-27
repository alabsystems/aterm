// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Crash recovery for account state, sync, renewal and pool materialization. The pending receipt fences readers;
//! replay installs exact admitted images, including their already allocated generations.
//! This is not atomicity for unrelated transactions or independent unlocked paired reads.

use super::*;
use serde::{Deserialize, Serialize};

const RECEIPT: &str = "sync-commit.json";
const WITNESS: &str = "sync-commit.seen";
const MAX_RECEIPT: u64 = 32 * 1024 * 1024;
const MAX_LEDGER: u64 = 8 * LEDGER_MAX_BYTES;

pub(in crate::harness) struct SyncTarget<'a> {
    pub path: &'a Path,
    pub before: Option<&'a str>,
    pub after: Option<&'a str>,
    pub mode: Option<u32>,
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
    if !file.metadata().map_err(|why| why.to_string())?.is_file() {
        return Err(format!("{}: sync file is not regular", path.display()));
    }
    let mut bytes = Vec::new();
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

fn durable_ancestry(dir: &Path) -> Result<(), String> {
    for ancestor in absolute(dir)?.ancestors() {
        aterm_durable::durable_dir(ancestor)
            .map_err(|why| format!("{}: {why}", ancestor.display()))?;
    }
    Ok(())
}

/// A retained absence changes no directory entry. Persist its existing ancestry
/// without creating the missing configuration directories.
fn publication_parent(path: &Path, absent: bool) -> &Path {
    let parent = path.parent().unwrap_or(Path::new("."));
    if absent {
        parent
            .ancestors()
            .find(|ancestor| ancestor.is_dir())
            .unwrap_or(parent)
    } else {
        parent
    }
}

impl Plan {
    fn paths(&self, store: &Store, roster: Option<&Path>) -> Result<Vec<(usize, PathBuf)>, String> {
        let mut paths = vec![(0, store.state_file()), (2, store.ledger_path())];
        if let Some(admitted) = &self.roster_path {
            if self.local_logins {
                return Err("local login receipt cannot name an external roster".into());
            }
            let mut roster =
                absolute(roster.ok_or("account recovery needs its admitted roster path")?)?;
            // New usage receipts pin the resolved source, including parent symlinks.
            // Older receipts retain their original absolute spelling.
            if roster.to_str() != Some(admitted.as_str()) {
                roster =
                    usage_target(Some(&roster))?.ok_or("account recovery lost its roster path")?;
            }
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
        Ok(paths)
    }

    fn validate(&self, store: &Store, roster: Option<&Path>) -> Result<(), String> {
        if self.roster_mode.is_some_and(|mode| mode & !0o777 != 0) {
            return Err("account sync receipt has invalid roster permissions".into());
        }
        let mut names = BTreeSet::new();
        for (_, path) in self.paths(store, roster)? {
            let name = usage_target(Some(&path))?.ok_or("account sync path is missing")?;
            if !names.insert(name) {
                return Err("account sync files must have distinct destinations".into());
            }
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
}

impl Store {
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
        self.recover_sync_at(held, None, &mut |_| Ok(()))?;
        let (mut state, read_note) = self.load(now)?;
        *note = read_note;
        if let Ok(id) = self.machine_id(&state.machine) {
            state.machine = id;
        }
        let before = state.clone();
        let events = edit(&mut state)?;
        let old_state = read(&self.state_file(), MAX_STATE_BYTES)?;
        let new_state = if state != before || (!events.is_empty() && old_state.is_none()) {
            drop_stale_sessions(&mut state, now);
            Some(state.to_json() + "\n")
        } else {
            old_state.clone()
        };
        let plan = Plan {
            local_logins: false,
            roster_path: None,
            roster_mode: None,
            state: Image {
                before: old_state,
                after: new_state,
            },
            logins: Image::default(),
            ledger: self.ledger_image(&events)?,
            roster: Image::default(),
            wake: Some(events.iter().any(Event::wakes)),
        };
        self.publish_plan(held, None, plan, &events, || Ok(()))?;
        Ok(events)
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

    fn transact_roster_records_held(
        &self,
        held: &Lock,
        roster: &Path,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&Roster, &mut State, &mut Logins) -> Result<Vec<Event>, String>,
        wake: WakeRule,
    ) -> Result<Vec<Event>, String> {
        let (target, mode) = super::super::cli::roster_target(roster)?;
        self.recover_sync_held(held, &target)?;
        let text = read(&target, MAX_ROSTER_BYTES)?
            .ok_or_else(|| format!("{}: account roster is missing", roster.display()))?;
        let raw = Roster::parse_for(self.cli, &text)?;
        self.transact_images_held(
            held,
            now,
            note,
            Publication {
                target: Some(SyncTarget {
                    path: &target,
                    before: Some(&text),
                    after: Some(&text),
                    mode,
                }),
                wake,
            },
            |state, logins| edit(&raw, state, logins),
            || Ok(()),
        )
    }

    /// Usage also works without configured rows. An absent file is a retained absence
    /// in the receipt, so a later configuration cannot silently admit old observations.
    pub(super) fn transact_usage_within(
        &self,
        ladder: &[std::time::Duration],
        captured: &UsageRoster,
        now: i64,
        edit: impl FnOnce(&Roster, &mut State, &mut Logins) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        let held = self
            .acquire_within(ladder)
            .map_err(|why| format!("{}: {why}", self.dir.display()))?
            .ok_or_else(|| LOCK_HELD.to_owned())?;
        let target = usage_target(captured.path.as_deref())?;
        if target != captured.target {
            return Err("usage roster target changed; nothing recorded".into());
        }
        let recovery_target = captured
            .path
            .as_deref()
            .map(super::super::cli::roster_target)
            .transpose()?
            .map(|(path, _)| path);
        self.recover_sync_at(&held, recovery_target.as_deref(), &mut |_| Ok(()))?;
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
        self.transact_images_held(
            &held,
            now,
            &mut None,
            Publication {
                target: target.as_ref().map(|path| SyncTarget {
                    path,
                    before: text.as_deref(),
                    after: text.as_deref(),
                    mode: None,
                }),
                wake: WakeRule::Events,
            },
            |state, logins| edit(&raw, state, logins),
            || Ok(()),
        )
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
            return Err("interrupted account sync; run `aterm harness accounts sync` to resume before using accounts".into());
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
        let stored = StoredReceipt {
            receipt: receipt.clone(),
            digest: receipt_digest(receipt)?,
        };
        let text = aterm_json::to_string(&stored).map_err(|why| why.to_string())?;
        if text.len() as u64 > MAX_RECEIPT {
            return Err("account sync receipt exceeds its limit".into());
        }
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
        if let Some(roster) = roster.filter(|_| plan.roster_path.is_some()) {
            let (target, _) = super::super::cli::roster_target(roster)?;
            self.recover_sync_held(&held, &target)
        } else {
            self.recover_sync_at(&held, None, &mut |_| Ok(()))
        }
    }

    /// Recover while the caller retains the account writer lock.
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
        _held: &Lock,
        roster: Option<&Path>,
        progress: &mut dyn FnMut(usize) -> Result<(), String>,
    ) -> Result<bool, String> {
        let Some(mut receipt) = self.receipt()? else {
            return Ok(false);
        };
        let Some(plan) = &receipt.plan else {
            return Ok(false);
        };
        plan.validate(self, roster)?;
        let paths = plan.paths(self, roster)?;
        let images = [&plan.state, &plan.logins, &plan.ledger, &plan.roster];
        let limits = [
            MAX_STATE_BYTES,
            MAX_LOGINS_BYTES,
            MAX_LEDGER,
            MAX_ROSTER_BYTES,
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
                let mode = if index == 3 {
                    plan.roster_mode
                } else {
                    Some(0o600)
                };
                super::super::upgrade_models::write_atomic_mode(path, text, mode)
                    .map_err(|why| format!("{}: {why}", path.display()))?;
            }
            // An earlier rename may have succeeded before its directory barrier failed.
            // Matching bytes still need that barrier before the receipt can complete.
            // A missing roster may also have missing parent directories. Preserve its
            // absence without creating configuration; persist the nearest existing parent.
            let barrier = publication_parent(path, image.after.is_none());
            aterm_durable::durable_dir(barrier)
                .map_err(|why| format!("{}: {why}", path.display()))?;
            progress(index + 1)?;
        }
        if plan.wake.unwrap_or(true) {
            self.touch_wake().map_err(|why| why.to_string())?;
        }
        aterm_durable::durable_dir(&self.dir).map_err(|why| why.to_string())?;
        progress(5)?;
        receipt.plan = None;
        self.save_receipt(&receipt)?;
        Ok(true)
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

    fn transact_images_held(
        &self,
        held: &Lock,
        now: i64,
        note: &mut Option<String>,
        publication: Publication<'_>,
        edit: impl FnOnce(&mut State, &mut Logins) -> Result<Vec<Event>, String>,
        auxiliary: impl FnOnce() -> Result<(), String>,
    ) -> Result<Vec<Event>, String> {
        let Publication { target, wake } = publication;
        let (mut state, state_note) = self.load(now)?;
        let (mut logins, login_note) = self.load_logins(now)?;
        *note = state_note.or(login_note);
        if let Ok(id) = self.machine_id(&state.machine) {
            state.machine = id;
        }
        let before = (state.clone(), logins.clone());
        let events = edit(&mut state, &mut logins)?;
        advance_renewal_generations(&before.1, &mut logins, &events)?;
        let old_state = read(&self.state_file(), MAX_STATE_BYTES)?;
        let old_logins = read(&self.logins_file(), MAX_LOGINS_BYTES)?;
        let new_state = if state != before.0 || (!events.is_empty() && old_state.is_none()) {
            drop_stale_sessions(&mut state, now);
            Some(state.to_json() + "\n")
        } else {
            old_state.clone()
        };
        let new_logins = if logins != before.1 {
            Some(logins.to_json() + "\n")
        } else {
            old_logins.clone()
        };
        let roster = target
            .as_ref()
            .map(|target| absolute(target.path))
            .transpose()?;
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
                WakeRule::Events => Some(events.iter().any(Event::wakes)),
            },
        };
        self.publish_plan(held, roster.as_deref(), plan, &events, auxiliary)?;
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

    fn publish_plan(
        &self,
        held: &Lock,
        roster: Option<&Path>,
        plan: Plan,
        events: &[Event],
        auxiliary: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        plan.validate(self, roster)?;
        let changed = [&plan.state, &plan.logins, &plan.ledger, &plan.roster]
            .iter()
            .any(|image| image.before != image.after);
        // Entropy and serialization failures are also admission failures, not partial commits.
        let receipt = Receipt {
            schema: 1,
            id: aterm_uds::rand::hex_token::<16>().map_err(|why| why.to_string())?,
            plan: Some(plan),
        };
        let encoded = aterm_json::to_string(&receipt).map_err(|why| why.to_string())?;
        if encoded.len() as u64 > MAX_RECEIPT {
            return Err("account sync receipt exceeds its limit".into());
        }
        auxiliary()?;
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
        durable_ancestry(&self.dir)?;
        if let Some(roster) = roster {
            let absent = receipt
                .plan
                .as_ref()
                .is_some_and(|plan| plan.roster.after.is_none());
            durable_ancestry(publication_parent(roster, absent))?;
        }
        self.retain_witness(&self.dir.join(WITNESS))
            .map_err(|why| why.to_string())?;
        self.save_receipt(&receipt)?;
        self.recover_sync_at(held, roster, &mut |_| Ok(()))?;
        Ok(())
    }
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
