// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local automatic-source retirement. This does not grant account or budget rights.

use super::*;
use serde::{Deserialize, Serialize};

pub(super) const FILE: &str = "source-policy.json";
pub(super) const WITNESS: &str = "source-policy.seen";
pub(super) const MAX_BYTES: u64 = 256 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    schema: u8,
    /// The logical legacy vault-fleet slot, independent of path or machine label.
    retired_at: i64,
}

/// An owned observation, including the exact representation fenced by the receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::harness) struct Snapshot {
    store: Store,
    image: Option<String>,
}

impl Snapshot {
    pub(in crate::harness) fn encoded(&self) -> Option<&[u8]> {
        self.image.as_deref().map(str::as_bytes)
    }

    pub(in crate::harness) fn retired(&self) -> bool {
        self.image.is_some()
    }

    pub(in crate::harness) fn check_current(&self) -> Result<(), String> {
        if self.store.source_policy()? != *self {
            return Err("automatic account source policy changed after capture".into());
        }
        Ok(())
    }
}

pub(super) fn validate(text: &str) -> Result<(), String> {
    if text.len() as u64 > MAX_BYTES {
        return Err("account source policy exceeds its limit".into());
    }
    let policy: Policy = aterm_json::from_str(text)
        .map_err(|why| format!("invalid account source policy: {why}"))?;
    if policy.schema != 1 {
        return Err("unsupported account source policy schema".into());
    }
    Ok(())
}

/// Compare resolved destinations and existing file identities, including hard links.
pub(in crate::harness) fn aliases(left: &Path, right: &Path) -> Result<bool, String> {
    if std::path::absolute(left).map_err(|why| why.to_string())?
        == std::path::absolute(right).map_err(|why| why.to_string())?
    {
        return Ok(true);
    }
    // A removed target of an excluded legacy entry cannot alias a live input.
    // Do not resolve its dangling symlink and let it disable unrelated sources.
    match std::fs::metadata(right) {
        Err(why) if why.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(why) => return Err(why.to_string()),
        Ok(_) => {}
    }
    let left = super::sync_commit::usage_target(Some(left))?.ok_or("missing source path")?;
    let right = super::sync_commit::usage_target(Some(right))?.ok_or("missing source path")?;
    if left == right {
        return Ok(true);
    }
    let file = match astream_sys::open_nonblocking(&left, astream_sys::Links::Refuse) {
        Ok(file) => file,
        Err(why) if why.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(why) => return Err(why.to_string()),
    };
    if !file.metadata().map_err(|why| why.to_string())?.is_file() {
        return Err("automatic source path is not a regular file".into());
    }
    match astream_sys::file_matches_path(&file, &right) {
        Ok(same) => Ok(same),
        Err(why) if why.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(why) => Err(why.to_string()),
    }
}

impl Store {
    pub(in crate::harness) fn source_policy_file(&self) -> PathBuf {
        self.dir.join(FILE)
    }

    pub(super) fn source_policy_witness(&self) -> PathBuf {
        self.dir.join(WITNESS)
    }

    /// A missing policy is initial only if no retirement witness remains.
    /// Readers fence the entire observation against pending and concurrent receipts.
    pub(in crate::harness) fn source_policy(&self) -> Result<Snapshot, String> {
        #[cfg(not(test))]
        let fenced = true;
        #[cfg(test)]
        let fenced = super::sync_commit::RETIREMENT_MUTANT.get() != 4;
        let receipt = if fenced {
            self.sync_read_receipt()?
        } else {
            None
        };
        let snapshot = self.source_policy_unfenced()?;
        if fenced {
            self.check_sync_read_receipt(&receipt)?;
        }
        Ok(snapshot)
    }

    /// Receipt validation already holds the writer and may be replaying a pending plan.
    pub(super) fn source_policy_unfenced(&self) -> Result<Snapshot, String> {
        let image = super::sync_commit::read(&self.source_policy_file(), MAX_BYTES)?;
        let witness = super::sync_commit::read(&self.source_policy_witness(), 0)?;
        if let Some(text) = &image {
            validate(text)?;
            if witness.is_none() {
                return Err("account source retirement witness is missing".into());
            }
        } else if witness.is_some() {
            return Err("account source policy is missing after retirement".into());
        }
        Ok(Snapshot {
            store: self.clone(),
            image,
        })
    }

    /// Only retirement is represented here. Ratification requires a separately
    /// guarded authenticated admission, never clearing this file or its witness.
    pub(super) fn retirement_image(&self, now: i64) -> Result<(Option<String>, String), String> {
        let before = self.source_policy()?.image;
        let keep = before.as_ref();
        #[cfg(test)]
        let keep = keep.filter(|_| super::sync_commit::RETIREMENT_MUTANT.get() != 5);
        let after = match keep {
            Some(text) => text.clone(),
            None => aterm_json::to_string(&Policy {
                schema: 1,
                retired_at: now,
            })
            .map_err(|why| why.to_string())?,
        };
        Ok((before, after))
    }
}
