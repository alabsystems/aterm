// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Crash recovery for sync, renewal and pool materialization. The pending receipt fences readers;
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
    pub after: &'a str,
    pub mode: Option<u32>,
}

enum WakeRule {
    Always,
    Events,
}

struct Publication<'a> {
    target: SyncTarget<'a>,
    wake: WakeRule,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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
    roster_path: String,
    roster_mode: Option<u32>,
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

fn durable_ancestry(dir: &Path) -> Result<(), String> {
    for ancestor in absolute(dir)?.ancestors() {
        aterm_durable::durable_dir(ancestor)
            .map_err(|why| format!("{}: {why}", ancestor.display()))?;
    }
    Ok(())
}

impl Plan {
    fn validate(&self, store: &Store, roster: &Path) -> Result<(), String> {
        if absolute(roster)?.to_str() != Some(self.roster_path.as_str())
            || self.roster_mode.is_some_and(|mode| mode & !0o777 != 0)
        {
            return Err("account sync recovery target differs from the admitted roster".into());
        }
        let paths = [
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            absolute(roster)?,
        ];
        let mut names = BTreeSet::new();
        for path in paths {
            let name = std::fs::canonicalize(&path).unwrap_or(absolute(&path)?);
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
        Roster::parse_for(
            store.cli,
            self.roster
                .after
                .as_deref()
                .ok_or("sync lacks its new roster")?,
        )?;
        Ok(())
    }
}

impl Store {
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
                target: SyncTarget {
                    path: &target,
                    before: Some(&text),
                    after: &text,
                    mode,
                },
                wake,
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

    /// Resume only at the caller's resolved roster, never a path supplied by the receipt.
    /// All images must still be admitted before any replacement is attempted.
    pub(in crate::harness) fn recover_sync_within(
        &self,
        ladder: &[std::time::Duration],
        roster: &Path,
    ) -> Result<bool, String> {
        if self.receipt()?.is_none_or(|receipt| receipt.plan.is_none()) {
            return Ok(false);
        }
        let held = self
            .acquire_within(ladder)
            .map_err(|why| why.to_string())?
            .ok_or_else(|| LOCK_HELD.to_owned())?;
        let (target, _) = super::super::cli::roster_target(roster)?;
        self.recover_sync_held(&held, &target)
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
        _held: &Lock,
        roster: &Path,
        progress: &mut dyn FnMut(usize) -> Result<(), String>,
    ) -> Result<bool, String> {
        let Some(mut receipt) = self.receipt()? else {
            return Ok(false);
        };
        let Some(plan) = &receipt.plan else {
            return Ok(false);
        };
        let roster = absolute(roster)?;
        plan.validate(self, &roster)?;
        let paths = [
            self.state_file(),
            self.logins_file(),
            self.ledger_path(),
            roster,
        ];
        let images = [&plan.state, &plan.logins, &plan.ledger, &plan.roster];
        let limits = [
            MAX_STATE_BYTES,
            MAX_LOGINS_BYTES,
            MAX_LEDGER,
            MAX_ROSTER_BYTES,
        ];
        for ((image, path), max) in images.iter().zip(&paths).zip(limits) {
            image.check(max)?;
            if !image.admits(&read(path, max)?) {
                return Err(format!(
                    "{} changed outside the interrupted sync; recovery preserved every file",
                    path.display()
                ));
            }
        }
        progress(0)?;
        for (index, ((image, path), max)) in images.iter().zip(&paths).zip(limits).enumerate() {
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
            aterm_durable::durable_dir(path.parent().unwrap_or(Path::new(".")))
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
                target,
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
        let old_ledger = read(&self.ledger_path(), MAX_LEDGER)?;
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
        let new_ledger = if events.is_empty() {
            old_ledger.clone()
        } else {
            let mut text = old_ledger.clone().unwrap_or_default();
            for event in &events {
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
        let roster = absolute(target.path)?;
        let plan = Plan {
            roster_path: roster.to_str().ok_or("roster path is not UTF-8")?.into(),
            roster_mode: target.mode,
            state: Image {
                before: old_state,
                after: new_state,
            },
            logins: Image {
                before: old_logins,
                after: new_logins,
            },
            ledger: Image {
                before: old_ledger,
                after: new_ledger,
            },
            roster: Image {
                before: target.before.map(str::to_owned),
                after: Some(target.after.into()),
            },
            wake: match wake {
                WakeRule::Always => None,
                WakeRule::Events => Some(events.iter().any(Event::wakes)),
            },
        };
        plan.validate(self, &roster)?;
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
        if !changed {
            return Ok(events);
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
        durable_ancestry(roster.parent().unwrap_or(Path::new(".")))?;
        self.retain_witness(&self.dir.join(WITNESS))
            .map_err(|why| why.to_string())?;
        self.save_receipt(&receipt)?;
        self.recover_sync_held(held, &roster)?;
        Ok(events)
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

#[cfg(test)]
#[path = "accounts_sync_commit_tests.rs"]
mod tests;
