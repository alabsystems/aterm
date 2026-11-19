// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Retained UI execution evidence in the SAME authenticated issuer journal.
//! This type has no NoExec, succession or reconstruction API. Domain-separated
//! recipe hashes cannot satisfy Restart receipt matching or mint an owner.
use super::*;
use crate::harness::codex_source_registry::{ServiceFrontend, ServiceIdentity};
use crate::harness::native_act::{Image, Process};

/// Original live Native operation's journal correlation. It contains no raw
/// argv/environment, reusable constructor or authority reconstructed from disk.
pub struct RetainedExecution {
    receipt: Receipt,
    epoch: Digest,
}
impl RetainedExecution {
    /// Reserve the existing issuer's complete Prepare/MayExecute/Seal budget.
    /// The caller retains its genuine service/foreground/image leases separately.
    ///
    /// # Errors
    /// Invalid selectors, changed source generation/image, conflicting history,
    /// unavailable identity/journal, or insufficient reserved closing capacity.
    pub fn prepare(
        service: &str,
        creator: &ServiceIdentity,
        frontend: &ServiceFrontend,
        operation: &str,
        capability: &str,
        target: &Image,
        command: &[u8],
    ) -> io::Result<Self> {
        let hex = |value: &str, size| aterm_codec::hex::is_lower(value, size);
        if !hex(service, 32)
            || !hex(operation, 32)
            || !hex(capability, 32)
            || command.is_empty()
            || command.len() > 256 * 1024 * 1024
        {
            return Err(invalid());
        }
        frontend.current().map_err(|_| invalid())?;
        let source = frontend.process();
        let identity =
            aterm_uds::process::read_identity(i32::try_from(source.pid).map_err(|_| invalid())?)
                .ok_or_else(invalid)?;
        if Process::observe(source.pid).as_ref() != Some(source) || &source.image == target {
            return Err(invalid());
        }
        let paths = Paths::locate().map_err(|_| invalid())?;
        let key = write_key(&paths)?;
        let description = aterm_json::to_vec(&aterm_json::json!({"version":1,"service":service,
            "creator":creator.journal_data(),"frontend":frontend.identity().journal_data(),
            "sourceGeneration":identity.token(),"targetPath":target.path,
            "targetDevice":target.device,"targetInode":target.inode,
            "rawCommandSha256":aterm_digest::Sha256::digest(command)}))
        .map_err(|_| invalid())?;
        let result = Self {
            receipt: Receipt {
                operation: key.operation(operation),
                prepare: key.digest(b"retained-exec-recipe/1\0", &description),
                capability: key.digest(b"retained-exec-capability/1\0", capability.as_bytes()),
            },
            epoch: key.digest(b"retained-exec-epoch/1\0", &creator.journal_data()),
        };
        append_at(&paths, &result, Event::Prepare, LIMITS)?;
        frontend.current().map_err(|_| invalid())?;
        if aterm_uds::process::read_identity(identity.pid) != Some(identity)
            || Process::observe(source.pid).as_ref() != Some(source)
        {
            return Err(invalid());
        }
        Ok(result)
    }
    /// Persist possible execution BEFORE grant or G1 command publication.
    /// An error leaves the caller's operation uncertain; it never permits NoExec.
    ///
    /// # Errors
    /// Missing, conflicting, closed, damaged or unavailable original history.
    pub fn may_execute(&self) -> io::Result<()> {
        append_at(
            &Paths::locate().map_err(|_| invalid())?,
            self,
            Event::MayExecute,
            LIMITS,
        )
        .map(|_| ())
    }
    /// Seal only after the original live Native owner has finished its operation.
    /// This records possible execution, never whether the command ran or App drew.
    ///
    /// # Errors
    /// Original history is unavailable/conflicting or never reached MayExecute.
    pub fn seal(&self) -> io::Result<()> {
        append_at(
            &Paths::locate().map_err(|_| invalid())?,
            self,
            Event::Seal,
            LIMITS,
        )
        .map(|_| ())
    }
}

fn append_at(
    paths: &Paths,
    execution: &RetainedExecution,
    event: Event,
    limits: Limits,
) -> io::Result<Observed> {
    let key = if event == Event::Prepare {
        write_key(paths)?
    } else {
        existing_key(paths)?
    };
    let (mut log, view) = if event == Event::Prepare {
        open(paths, &key, limits)?
    } else {
        open_existing(paths, &key, limits)?
    };
    let old = view.entries.get(&execution.receipt.operation);
    if let Some(old) = old {
        if old.receipt != execution.receipt
            || old.epoch != Some(execution.epoch)
            || old.recipients.is_some()
            || old.account.is_some()
            || old.predecessor.is_some()
        {
            return Err(invalid());
        }
        if event.already(old.state) {
            return Ok(old.observed);
        }
    }
    // No retained NoExec/Reject event exists. Seal is only a MayExecute close;
    // prepared-but-unresolved operations conservatively retain their reservation.
    if !matches!(event, Event::Prepare | Event::MayExecute | Event::Seal)
        || (event == Event::Seal && old.is_none_or(|old| old.state != IssuerState::MayExecute))
    {
        return Err(invalid());
    }
    event.advance(old.map(|old| old.state))?;
    if !view.can_append(Some(event), limits) {
        return Err(io::Error::other(
            "native issuer closing capacity is unavailable",
        ));
    }
    let observed = Observed {
        sequence: view.sequence,
        recorded_ms: aterm_time::unix_millis().ok_or_else(invalid)?,
    };
    log.append(
        ISSUER_SUBJECT,
        view.encode_event(
            &key,
            &execution.receipt,
            execution.epoch,
            event,
            observed.recorded_ms,
        ),
    )?;
    Ok(observed)
}

#[cfg(test)]
#[path = "native_retained_receipts_tests.rs"]
mod tests;
