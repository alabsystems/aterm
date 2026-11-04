// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded root references and immutable attempt records. These files record
//! custody; they never replace retained kernel evidence or grant native effects.

use super::*;
use std::io::{Read as _, Write as _};

const MAX_RECORD: usize = 32 * 1024;

#[cfg(test)]
#[path = "codex_renewal_attempts_tests.rs"]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptRef {
    pub id: String,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub root: String,
    pub reference: AttemptRef,
    pub parent: AttemptRef,
    pub native_predecessor: String,
    pub native_prepare: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationKind {
    OwnerStop,
    NewPersonTask,
    ChangedRecord,
    ChangedSelection,
    PolicyRevoked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cancellation {
    pub root: String,
    pub intent: Intent,
    pub reason: CancellationKind,
    pub human_seq: u64,
    pub reached_seq: u64,
}

/// The account writer acquired for this exact environment. Its private
/// constructor prevents callers from publishing with an unrelated store lock.
pub struct ReceiptWriter<'a> {
    env: &'a Env,
    _guard: &'a accounts::Lock,
}

/// The trusted native host's retained kernel evidence for a cancelled attempt.
/// Implementations must recheck process-group closure, drained input, and the
/// absence of an executed or admitted successor for this exact preparation.
/// A receipt, PID lookup, or serialized observation alone is not this proof.
pub trait NativeCancellationProof {
    fn closed_without_successor(&self, prepare: &super::super::native_act::Prepare) -> bool;
}

impl<'a> ReceiptWriter<'a> {
    pub(super) fn held(env: &'a Env, guard: &'a accounts::Lock) -> Self {
        Self { env, _guard: guard }
    }

    pub(super) fn release_unexecuted_rotation(
        &self,
        receipt: &Receipt,
        target: &str,
    ) -> Result<(), &'static str> {
        let binding = super::super::codex_rotation::receipt_binding(receipt)?;
        let store = self
            .env
            .accounts_store_for(accounts::Cli::Codex)
            .ok_or("store")?;
        store
            .transact_logins_held(self._guard, self.env.now, &mut None, |state, logins| {
                match logins.native_rotations.get(&receipt.id) {
                    None => return Ok(Vec::new()),
                    Some(current) if current != &binding => {
                        return Err("rotation binding changed".into());
                    }
                    Some(_) => {}
                }
                Ok(if state.pinned {
                    Vec::new()
                } else {
                    accounts::strike(
                        state,
                        target,
                        &receipt.sid,
                        &accounts::Strike::NotMoved(
                            "native reservation cancelled before execution".into(),
                        ),
                        self.env.now,
                    )
                })
            })
            .map_err(|_| "rotation-reservation-release")?;
        self.retire_pool_record(receipt)?;
        Ok(())
    }

    pub(super) fn prove_completed_rotation(
        &self,
        receipt: &Receipt,
        target: &str,
    ) -> Result<(), &'static str> {
        if receipt.intent.purpose != super::Purpose::WeeklyQuota
            || receipt.phase != Phase::Consumed
            || receipt.accepted_turn.as_deref() != Some(receipt.intent.wall.as_str())
            || super::read_receipt(self.env, &receipt.id).as_ref() != Some(receipt)
        {
            return Err("rotation-not-consumed");
        }
        let store = self
            .env
            .accounts_store_for(accounts::Cli::Codex)
            .ok_or("store")?;
        store
            .transact_logins_held(self._guard, self.env.now, &mut None, |state, _| {
                if state.active.as_deref() != Some(target) {
                    return Err("completed rotation is no longer selected".into());
                }
                Ok(accounts::prove(state, target, self.env.now)
                    .into_iter()
                    .collect())
            })
            .map_err(|_| "rotation-completion")?;
        Ok(())
    }

    /// Retire pool deduplication only after the matching root is durably
    /// consumed, or retired without ever attempting native execution. A crash
    /// between terminal publication and cleanup is retried by the next reader.
    pub fn retire_pool_record(&self, expected: &Receipt) -> Result<bool, &'static str> {
        let terminal = (expected.phase == Phase::Consumed
            && expected.accepted_turn.as_deref() == Some(expected.intent.wall.as_str()))
            || (expected.phase == Phase::Retired && !expected.launch_attempted);
        self.retire_terminal_pool_record(expected, terminal)
    }

    /// Retire a cancelled, signalled attempt without rewriting its effect history.
    pub fn retire_cancelled_pool_record(
        &self,
        expected: &Receipt,
        proof: &impl NativeCancellationProof,
    ) -> Result<bool, &'static str> {
        if expected.phase != Phase::Retired
            || !expected.revoked
            || expected.current_attempt.is_some()
            || expected.candidate.is_some()
            || expected.successor_pid.is_some()
            || expected.successor_birth.is_some()
            || expected.accepted_turn.is_some()
            || read_cancellation(self.env, expected)?.is_none()
        {
            return Err("cancelled-rotation-not-terminal");
        }
        let Some(super::super::native_act::Request::Prepare(prepare)) =
            super::super::native_act::Request::parse(&expected.native_prepare)
        else {
            return Err("cancelled-rotation-recipe");
        };
        self.retire_terminal_pool_record(expected, proof.closed_without_successor(&prepare))
    }

    /// A launched rollback retires only with the matching native completion
    /// ledger. A retired receipt or a captured launcher alone is insufficient.
    pub fn retire_rolled_back_pool_record(
        &self,
        expected: &Receipt,
        opts: &super::super::upgrade_drive::Opts,
    ) -> Result<bool, &'static str> {
        if opts.state != self.env.state || self.env.home.as_ref() != Some(&opts.home) {
            return Err("rollback-store-mismatch");
        }
        let recipe = match read_active_attempt(self.env, expected)? {
            Some(attempt) => attempt.native_prepare,
            None => expected.native_prepare.clone(),
        };
        let Some(super::super::native_act::Request::Prepare(prepare)) =
            super::super::native_act::Request::parse(&recipe)
        else {
            return Err("rollback-recipe");
        };
        let state = super::super::upgrade_drive::load(opts, &format!("renewal-{}", prepare.id))
            .ok_or("rollback-completion-missing")?;
        let terminal = state.native_coordination
            && state.native_rollback
            && state.native_completed_prepare.as_deref() == Some(&prepare)
            && state
                .native_observation
                .as_ref()
                .is_some_and(|observed| expected.matches_completed_rollback(&prepare, observed));
        self.retire_terminal_pool_record(expected, terminal)
    }

    fn retire_terminal_pool_record(
        &self,
        expected: &Receipt,
        terminal: bool,
    ) -> Result<bool, &'static str> {
        if expected.intent.purpose != super::Purpose::WeeklyQuota {
            return Ok(false);
        }
        if !terminal || super::read_receipt(self.env, &expected.id).as_ref() != Some(expected) {
            return Err("rotation-not-terminal");
        }
        let binding = super::super::codex_rotation::receipt_binding(expected)?;
        let store = self
            .env
            .accounts_store_for(accounts::Cli::Codex)
            .ok_or("store")?;
        let mut removed = false;
        store
            .transact_logins_held(self._guard, self.env.now, &mut None, |_, logins| {
                match logins.native_rotations.get(&expected.id) {
                    None => return Ok(Vec::new()),
                    Some(current) if current != &binding => {
                        return Err("rotation binding changed".into());
                    }
                    Some(_) => {}
                }
                logins.pending_native_retirement = Some(accounts::NativeRotationRetirement::new(
                    &expected.id,
                    &binding,
                ));
                logins.native_rotations.remove(&expected.id);
                removed = true;
                Ok(Vec::new())
            })
            .map_err(|_| "rotation-retirement")?;
        Ok(removed)
    }

    /// Construct a proposed immutable child. The native predecessor can be the
    /// current attempt, or its still-unconsumed predecessor when the current
    /// journal entry expired without admission. Only retained native proof can
    /// establish which of those is actually available to the sealed factory.
    /// # Errors
    /// The root is terminal, a parent record is missing, the recipe changed, or
    /// the generation/identifier/deadline cannot name an immutable fresh attempt.
    pub fn propose(
        &self,
        expected: &Receipt,
        predecessor: &str,
        prepare: &super::super::native_act::Prepare,
    ) -> Result<Attempt, &'static str> {
        use super::super::native_act::Request as NativeRequest;
        if expected.phase != Phase::Committing
            || !expected.launch_attempted
            || expected.revoked
            || expected.accepted_turn.is_some()
        {
            return Err("attempt-intent-terminal");
        }
        let parent = current_reference(expected);
        let previous = read_active_attempt(self.env, expected)?;
        if predecessor != parent.id
            && previous
                .as_ref()
                .is_none_or(|attempt| predecessor != attempt.native_predecessor)
        {
            return Err("attempt-predecessor");
        }
        let NativeRequest::Prepare(original) =
            NativeRequest::parse(&expected.native_prepare).ok_or("attempt-original-plan")?
        else {
            return Err("attempt-original-plan");
        };
        let prior = previous
            .as_ref()
            .map_or(expected.native_prepare.as_str(), |a| {
                a.native_prepare.as_str()
            });
        let NativeRequest::Prepare(prior) =
            NativeRequest::parse(prior).ok_or("attempt-parent-plan")?
        else {
            return Err("attempt-parent-plan");
        };
        if original.id != expected.id
            || !same_recipe(&original, prepare)
            || prepare.id == expected.id
            || prepare.id == parent.id
            || prepare.deadline_us <= prior.deadline_us
            || !aterm_codec::hex::is_lower(&prepare.id, 32)
            || prepare.expected_human_seq.is_none()
        {
            return Err("attempt-recipe-changed");
        }
        let reference = AttemptRef {
            id: prepare.id.clone(),
            generation: parent
                .generation
                .checked_add(1)
                .ok_or("attempt-generation-exhausted")?,
        };
        let native_prepare = NativeRequest::prepare(prepare.clone())
            .encode()
            .ok_or("attempt-plan")?;
        Ok(Attempt {
            root: expected.id.clone(),
            reference,
            parent,
            native_predecessor: predecessor.into(),
            native_prepare,
        })
    }

    /// Persist an immutable attempt, flush its directory, then CAS-publish the
    /// bounded root reference. A failed publication never dispatches the child.
    /// # Errors
    /// The root changed or was revoked, an immutable record conflicts, or a
    /// file/directory flush or the root publication failed. No dispatch occurs here.
    pub fn publish(&self, expected: &Receipt, attempt: &Attempt) -> Result<Receipt, &'static str> {
        self.publish_using(expected, attempt, |receipt| {
            write_receipt(self.env, receipt)
        })
    }

    fn publish_using(
        &self,
        expected: &Receipt,
        attempt: &Attempt,
        publish: impl FnOnce(&Receipt) -> Result<(), &'static str>,
    ) -> Result<Receipt, &'static str> {
        if read_cancellation(self.env, expected)?.is_some() {
            return Err("attempt-revoked");
        }
        let current = read_receipt(self.env, &expected.id).ok_or("attempt-receipt-unavailable")?;
        if current.revoked {
            return Err("attempt-revoked");
        }
        if current.current_attempt.as_ref() == Some(&attempt.reference)
            && read_active_attempt(self.env, &current)?.as_ref() == Some(attempt)
            && same_intent(expected, &current)
        {
            // Repair an uncertain directory-flush acknowledgement before any retry.
            flush_receipt_dir(self.env, expected)?;
            return Ok(current);
        }
        if &current != expected {
            return Err("attempt-receipt-changed");
        }
        let super::super::native_act::Request::Prepare(prepare) =
            super::super::native_act::Request::parse(&attempt.native_prepare)
                .ok_or("attempt-plan")?
        else {
            return Err("attempt-plan");
        };
        if self.propose(expected, &attempt.native_predecessor, &prepare)? != *attempt {
            return Err("attempt-link-changed");
        }
        let path = attempt_path(self.env, &attempt.root, &attempt.reference.id)?;
        write_immutable(&path, attempt)?;
        let mut next = current;
        next.current_attempt = Some(attempt.reference.clone());
        next.candidate = None;
        publish(&next)?;
        Ok(next)
    }

    /// A separate, durable tombstone cannot be erased by a stale root writer.
    /// Preserve the first reason, and never turn Consumed observation into new authority.
    /// # Errors
    /// The logical intent changed, custody cannot be read, or durable tombstone
    /// and root projection publication failed. A completed tombstone is retained.
    pub fn cancel(
        &self,
        expected: &Receipt,
        reason: CancellationKind,
        human_seq: u64,
        reached_seq: u64,
    ) -> Result<Option<Cancellation>, &'static str> {
        let current = read_receipt(self.env, &expected.id).ok_or("cancel-receipt-unavailable")?;
        if !same_intent(expected, &current) {
            return Err("cancel-intent-changed");
        }
        if current.phase == Phase::Consumed {
            return Ok(None);
        }
        let cancellation = if let Some(existing) = read_cancellation(self.env, &current)? {
            existing
        } else {
            let cancellation = Cancellation {
                root: current.id.clone(),
                intent: current.intent.clone(),
                reason,
                human_seq,
                reached_seq,
            };
            let path = cancellation_path(self.env, &current.id)?;
            let text = encoded(&cancellation)?;
            private_parent(&path)?;
            super::super::upgrade_models::write_atomic_mode(&path, &text, Some(0o600))
                .map_err(|_| "cancel-write")?;
            flush_parent(&path)?;
            cancellation
        };
        let mut retired = current;
        retired.revoked = true;
        retired.phase = Phase::Retired;
        write_receipt(self.env, &retired)?;
        Ok(Some(cancellation))
    }
}

/// A receipt-only transaction needs no sign-in or auth-file lock. Successor
/// selection exposes the same writer through `with_successor_writer` instead.
/// # Errors
/// The configured store cannot be named or locked, or the callback failed.
pub fn with_receipt_writer<T>(
    env: &Env,
    use_writer: impl FnOnce(&ReceiptWriter<'_>) -> Result<T, &'static str>,
) -> Result<Option<T>, &'static str> {
    let store = env
        .accounts_store_for(accounts::Cli::Codex)
        .ok_or("store-unavailable")?;
    let Some(guard) = store.acquire_bounded().map_err(|_| "receipt-lock")? else {
        return Ok(None);
    };
    use_writer(&ReceiptWriter::held(env, &guard)).map(Some)
}

/// Read an immutable attempt by its exact reference. This metadata never
/// establishes that a native operation was admitted or closed.
/// # Errors
/// The record is missing, unsafe, malformed, or belongs to another root/reference.
pub fn read_attempt(
    env: &Env,
    root: &str,
    reference: &AttemptRef,
) -> Result<Attempt, &'static str> {
    let path = attempt_path(env, root, &reference.id)?;
    let attempt: Attempt = read_record(&path)?.ok_or("attempt-missing")?;
    if attempt.root != root || &attempt.reference != reference {
        return Err("attempt-reference-changed");
    }
    Ok(attempt)
}

/// Read one bounded active record and its immediate parent, validating their
/// exact references and frozen recipe without traversing unbounded history.
/// # Errors
/// A referenced record is missing, linked incorrectly, unsafe, too large, or invalid.
pub fn read_active_attempt(env: &Env, receipt: &Receipt) -> Result<Option<Attempt>, &'static str> {
    let Some(reference) = &receipt.current_attempt else {
        return Ok(None);
    };
    let path = attempt_path(env, &receipt.id, &reference.id)?;
    let attempt: Attempt = read_record(&path)?.ok_or("attempt-missing")?;
    if attempt.root != receipt.id
        || &attempt.reference != reference
        || reference.generation == 0
        || reference.id == receipt.id
        || attempt.parent.generation.checked_add(1) != Some(reference.generation)
        || !aterm_codec::hex::is_lower(&attempt.parent.id, 32)
        || !aterm_codec::hex::is_lower(&attempt.native_predecessor, 32)
    {
        return Err("attempt-shape");
    }
    let super::super::native_act::Request::Prepare(prepare) =
        super::super::native_act::Request::parse(&attempt.native_prepare).ok_or("attempt-plan")?
    else {
        return Err("attempt-plan");
    };
    if prepare.id != reference.id {
        return Err("attempt-plan-id");
    }
    let super::super::native_act::Request::Prepare(original) =
        super::super::native_act::Request::parse(&receipt.native_prepare)
            .ok_or("attempt-original-plan")?
    else {
        return Err("attempt-original-plan");
    };
    if original.id != receipt.id || !same_recipe(&original, &prepare) {
        return Err("attempt-recipe-changed");
    }
    let (parent_prepare, inherited_predecessor) = if attempt.parent.generation == 0 {
        if attempt.parent.id != receipt.id {
            return Err("attempt-parent-root");
        }
        (receipt.native_prepare.clone(), receipt.id.clone())
    } else {
        let parent_path = attempt_path(env, &receipt.id, &attempt.parent.id)?;
        let parent: Attempt = read_record(&parent_path)?.ok_or("attempt-parent-missing")?;
        if parent.root != receipt.id || parent.reference != attempt.parent {
            return Err("attempt-parent-changed");
        }
        (parent.native_prepare, parent.native_predecessor)
    };
    let super::super::native_act::Request::Prepare(parent_prepare) =
        super::super::native_act::Request::parse(&parent_prepare).ok_or("attempt-parent-plan")?
    else {
        return Err("attempt-parent-plan");
    };
    if !same_recipe(&original, &parent_prepare) {
        return Err("attempt-parent-recipe");
    }
    if parent_prepare.id != attempt.parent.id
        || prepare.deadline_us <= parent_prepare.deadline_us
        || (attempt.native_predecessor != attempt.parent.id
            && attempt.native_predecessor != inherited_predecessor)
    {
        return Err("attempt-parent-link");
    }
    Ok(Some(attempt))
}

/// Read the separate immutable logical cancellation. Unreadable custody is an
/// error, never evidence that no cancellation exists.
/// # Errors
/// The record cannot be read safely or belongs to a different logical intent.
pub fn read_cancellation(
    env: &Env,
    receipt: &Receipt,
) -> Result<Option<Cancellation>, &'static str> {
    let path = cancellation_path(env, &receipt.id)?;
    let cancellation: Option<Cancellation> = read_record(&path)?;
    if cancellation
        .as_ref()
        .is_some_and(|c| c.root != receipt.id || c.intent != receipt.intent)
    {
        return Err("cancel-intent-changed");
    }
    Ok(cancellation)
}

pub(super) fn read_root(env: &Env, id: &str) -> Result<Option<Receipt>, &'static str> {
    read_record(&receipt_path(env, id).ok_or("receipt-id")?)
}

fn same_recipe(
    original: &super::super::native_act::Prepare,
    proposed: &super::super::native_act::Prepare,
) -> bool {
    let mut normalized = proposed.clone();
    normalized.id.clone_from(&original.id);
    normalized.revision = original.revision;
    normalized.expected_human_seq = original.expected_human_seq;
    normalized.deadline_us = original.deadline_us;
    normalized == *original
}

fn current_reference(receipt: &Receipt) -> AttemptRef {
    receipt
        .current_attempt
        .clone()
        .unwrap_or_else(|| AttemptRef {
            id: receipt.id.clone(),
            generation: 0,
        })
}

pub(super) fn same_intent(left: &Receipt, right: &Receipt) -> bool {
    left.id == right.id
        && left.intent == right.intent
        && left.destination == right.destination
        && left.sid == right.sid
        && left.old_pid == right.old_pid
        && left.old_birth == right.old_birth
        && left.claim_revision == right.claim_revision
        && left.human_seq == right.human_seq
        && left.expires_ms == right.expires_ms
        && left.old_auth == right.old_auth
        && left.new_auth == right.new_auth
        && left.generation == right.generation
        && left.row_stamp == right.row_stamp
        && left.native_prepare == right.native_prepare
}

fn attempt_path(env: &Env, root: &str, id: &str) -> Result<PathBuf, &'static str> {
    if !aterm_codec::hex::is_lower(id, 32) {
        return Err("attempt-id");
    }
    let root_path = receipt_path(env, root).ok_or("attempt-root-id")?;
    Ok(root_path
        .parent()
        .ok_or("attempt-root-path")?
        .join(format!("{root}.attempts"))
        .join(format!("{id}.json")))
}

fn cancellation_path(env: &Env, root: &str) -> Result<PathBuf, &'static str> {
    let root_path = receipt_path(env, root).ok_or("cancel-root-id")?;
    Ok(root_path
        .parent()
        .ok_or("cancel-root-path")?
        .join(format!("{root}.revoked.json")))
}

fn encoded<T: Serialize>(value: &T) -> Result<String, &'static str> {
    let text = aterm_json::to_string(value).map_err(|_| "journal-encoding")?;
    if text.len() > MAX_RECORD {
        return Err("journal-size");
    }
    Ok(text)
}

fn private_parent(path: &Path) -> Result<(), &'static str> {
    let parent = path.parent().ok_or("journal-path")?;
    aterm_types::fs_restricted::ensure_private_dir(parent).map_err(|_| "journal-directory")?;
    // Repair a prior uncertain mkdir acknowledgement as well as a new directory.
    if let Some(grandparent) = parent.parent() {
        aterm_durable::Flush::Durable
            .dir(grandparent)
            .map_err(|_| "journal-parent-flush")?;
    }
    Ok(())
}

fn flush_parent(path: &Path) -> Result<(), &'static str> {
    aterm_durable::Flush::Durable
        .dir(path.parent().ok_or("journal-path")?)
        .map_err(|_| "journal-directory-flush")
}

fn flush_receipt_dir(env: &Env, receipt: &Receipt) -> Result<(), &'static str> {
    flush_parent(&receipt_path(env, &receipt.id).ok_or("receipt-id")?)
}

fn write_immutable<T: Serialize>(path: &Path, value: &T) -> Result<(), &'static str> {
    let text = encoded(value)?;
    private_parent(path)?;
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    match open.open(path) {
        Ok(mut file) => {
            file.write_all(text.as_bytes())
                .map_err(|_| "journal-write")?;
            crate::durable::sync_all(&file).map_err(|_| "journal-file-flush")?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let (file, bytes) = read_retained(path)?.ok_or("journal-existing-missing")?;
            if bytes.as_slice() != text.as_bytes() {
                return Err("journal-immutable-conflict");
            }
            // A previous caller may have lost the file-flush acknowledgement,
            // not merely the directory acknowledgement. Repair both before CAS.
            crate::durable::sync_all(&file).map_err(|_| "journal-file-flush")?;
        }
        Err(_) => return Err("journal-create"),
    }
    flush_parent(path)
}

fn read_record<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, &'static str> {
    read_bytes(path)?
        .map(|bytes| aterm_json::from_slice(&bytes).map_err(|_| "journal-shape"))
        .transpose()
}

fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>, &'static str> {
    Ok(read_retained(path)?.map(|(_, bytes)| bytes))
}

fn read_retained(path: &Path) -> Result<Option<(std::fs::File, Vec<u8>)>, &'static str> {
    let mut open = std::fs::OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        open.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let mut file = match open.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("journal-open"),
    };
    let metadata = file.metadata().map_err(|_| "journal-metadata")?;
    if !metadata.is_file() || metadata.len() > MAX_RECORD as u64 {
        return Err("journal-size");
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if metadata.file_attributes() & 0x0000_0400 != 0 {
            return Err("journal-reparse-point");
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: getuid takes no pointers and cannot fail.
        let uid = unsafe { libc::getuid() };
        if metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err("journal-permissions");
        }
    }
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take((MAX_RECORD + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "journal-read")?;
    if bytes.len() > MAX_RECORD {
        return Err("journal-size");
    }
    Ok(Some((file, bytes)))
}
