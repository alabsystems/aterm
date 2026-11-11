// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Account-writer-owned references from renewal intents to retained roots.

use super::*;

/// A renewal intent's index name, resolved only inside the writer's account store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexKey {
    purpose: Purpose,
    digest: String,
}

impl IndexKey {
    pub fn login(pid: u32, start: &str, wall: &str, generation: u64) -> Self {
        Self::new(
            Purpose::Login,
            format!("{pid}:{start}:{wall}:{generation}").as_bytes(),
        )
    }

    /// # Errors
    /// The intent cannot be encoded or is not a quota intent.
    pub fn quota(intent: &Intent, pid: u32, birth: u64, start: &str) -> Result<Self, &'static str> {
        if intent.purpose != Purpose::WeeklyQuota {
            return Err("index-purpose");
        }
        let key = aterm_json::to_string(&(intent, pid, birth, start)).map_err(|_| "index-key")?;
        Ok(Self::new(Purpose::WeeklyQuota, key.as_bytes()))
    }

    fn new(purpose: Purpose, key: &[u8]) -> Self {
        Self {
            purpose,
            digest: aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(key)),
        }
    }

    fn path(&self, env: &Env) -> Result<PathBuf, &'static str> {
        let prefix = match self.purpose {
            Purpose::Login => "intent",
            Purpose::WeeklyQuota => "quota-intent",
        };
        Ok(env
            .accounts_store_for(accounts::Cli::Codex)
            .ok_or("index-store")?
            .dir
            .join("renewal-follow")
            .join(format!("{prefix}-{}", self.digest)))
    }

    /// Observe a reference without acknowledging it. Callers must repeat the
    /// lookup under the account writer before using it as a durable result.
    /// # Errors
    /// The store is unavailable or the retained index is unsafe or malformed.
    pub fn observe(&self, env: &Env) -> Result<Option<String>, &'static str> {
        Ok(read_index_file(&self.path(env)?, false)?.map(|(_, id)| id))
    }
}

fn read_index_file(
    path: &Path,
    acknowledge: bool,
) -> Result<Option<(std::fs::File, String)>, &'static str> {
    let Some((file, bytes)) = read_retained(path, acknowledge)? else {
        return Ok(None);
    };
    let id = String::from_utf8(bytes).map_err(|_| "index-shape")?;
    if !aterm_codec::hex::is_lower(&id, 32) {
        return Err("index-shape");
    }
    Ok(Some((file, id)))
}

impl ReceiptWriter<'_> {
    /// Load and acknowledge an index and its current root under the account
    /// writer. Missing roots and unsafe files are uncertainty, never absence.
    /// # Errors
    /// The index or root is invalid, changed, or cannot be acknowledged.
    pub fn read_indexed(&self, key: &IndexKey) -> Result<Option<Receipt>, &'static str> {
        let path = key.path(self.env)?;
        let Some((file, id)) = read_index_file(&path, true)? else {
            return Ok(None);
        };
        let receipt = read_receipt(self.env, &id).ok_or("indexed-receipt")?;
        if receipt.intent.purpose != key.purpose {
            return Err("index-purpose");
        }
        crate::durable::sync_all(&file).map_err(|_| "index-file-flush")?;
        self.acknowledge(&receipt)?;
        if key.observe(self.env)?.as_deref() != Some(&id) {
            return Err("index-changed");
        }
        Ok(Some(receipt))
    }

    /// Publish an initial root and then its index. The expected prior receipt
    /// is an exact compare-and-swap witness; only retired, replaceable roots may
    /// yield their index. An exact retry repairs the durability acknowledgement.
    /// # Errors
    /// The witness changed, a retained root cannot be replaced, the initial
    /// receipt is invalid, or either publication cannot be acknowledged.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ReceiptIndex",
            action = "Scenario",
            reason = "The filesystem and caller supply the retained index and prior witness"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ReceiptIndex",
            action = "Failed",
            reason = "The filesystem refuses a directory acknowledgement"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ReceiptIndex",
            action = "Publish",
            project = "aterm_agent::harness::codex_renewal::attempts::index::tests::transition"
        )
    )]
    pub fn publish_indexed(
        &self,
        key: &IndexKey,
        prior: Option<&Receipt>,
        initial: &Receipt,
    ) -> Result<(), &'static str> {
        if initial.intent.purpose != key.purpose {
            return Err("index-purpose");
        }
        let current = self.read_indexed(key)?;
        if current.as_ref() == Some(initial) {
            return self.create(initial);
        }
        if current.as_ref() != prior {
            return Err("index-changed");
        }
        if current.as_ref().is_some_and(|old| !old.may_replace()) {
            return Err("index-not-replaceable");
        }
        self.create(initial)?;
        let path = key.path(self.env)?;
        super::super::super::atomic_write::write_atomic_mode(&path, &initial.id, Some(0o600))
            .map_err(|_| "index-write")?;
        flush_parent(&path)
    }
}

#[cfg(test)]
#[path = "codex_renewal_index_tests.rs"]
mod tests;
