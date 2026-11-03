// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Persistable closure observations. A reference names an original claim epoch; it is
//! never a guard descriptor, writer, birth receipt or proof that anything has ended.

use crate::ledger::ClaimGuardIdentity;
use aterm_json::Value;
use std::io;

fn bad() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid cohort closure reference",
    )
}

fn tag(text: &str) -> bool {
    text.len() == 32
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Identity of the namespace, nonreused claim epoch and original guard captured from an
/// actual held claim. Decoding this data grants no mutation or process authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CohortClosureRef {
    namespace: String,
    marker: String,
    epoch: String,
    guard: ClaimGuardIdentity,
}

impl CohortClosureRef {
    pub(crate) fn captured(
        namespace: &str,
        marker: &str,
        epoch: &str,
        guard: ClaimGuardIdentity,
    ) -> io::Result<Self> {
        if !tag(namespace)
            || !tag(epoch)
            || crate::ledger::Record::marker_holder(marker).is_none()
            || guard.inode == 0
        {
            return Err(bad());
        }
        Ok(Self {
            namespace: namespace.into(),
            marker: marker.into(),
            epoch: epoch.into(),
            guard,
        })
    }

    /// Namespace nonce captured with the original claim; not a live namespace permit.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// The original record's cover marker.
    pub fn marker(&self) -> &str {
        &self.marker
    }

    /// Random immutable claim epoch, which distinguishes marker/inode reuse after GC.
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// Device/inode of the original held guard, not a fresh pathname's identity.
    pub fn guard(&self) -> ClaimGuardIdentity {
        self.guard
    }

    #[cfg(unix)]
    pub(crate) fn key(&self) -> String {
        format!("{}.{}.noindex", self.marker, self.epoch)
    }

    /// Encode an observation for an owning lifecycle journal. This data is not closure proof.
    pub fn to_json(&self) -> Value {
        aterm_json::json!({"v":3u64,"namespace":self.namespace,"marker":self.marker,
            "epoch":self.epoch,"guard":{"device":self.guard.device,"inode":self.guard.inode}})
    }

    /// Decode only the exact reference shape; actual query/reap still validates the current
    /// physical namespace, epoch and receipt. Missing evidence never becomes Closed.
    /// # Errors
    /// The version, shape, namespace/epoch tags, marker or original guard are invalid.
    pub fn from_json(value: &Value) -> io::Result<Self> {
        if value.as_object().is_none_or(|m| m.len() != 5)
            || value.get("v").and_then(Value::as_u64) != Some(3)
        {
            return Err(bad());
        }
        let text = |key| value.get(key).and_then(Value::as_str).ok_or_else(bad);
        let guard = value.get("guard").ok_or_else(bad)?;
        if guard.as_object().is_none_or(|m| m.len() != 2) {
            return Err(bad());
        }
        Self::captured(
            text("namespace")?,
            text("marker")?,
            text("epoch")?,
            ClaimGuardIdentity {
                device: guard
                    .get("device")
                    .and_then(Value::as_u64)
                    .ok_or_else(bad)?,
                inode: guard.get("inode").and_then(Value::as_u64).ok_or_else(bad)?,
            },
        )
    }
}
