// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Journal correlation only. Neither deserialization nor a native operation ID
//! authorizes HTTP: the actual natural read permit and canonical owner custody
//! must still admit each provider contact.
use super::*;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum JournalOperation {
    Session(Operation),
    NativeQuota(NativeQuotaOperation),
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativeQuotaOperation {
    pub(super) id: String,
    pub(super) expected_digest: String,
    pub(super) credential_generation: u64,
    pub(super) native_quota: NativeQuotaContext,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativeQuotaContext {
    pub(super) schema: u32,
    pub(super) read_context_digest: String,
}

impl From<Operation> for JournalOperation {
    fn from(operation: Operation) -> Self {
        Self::Session(operation)
    }
}
impl JournalOperation {
    pub(super) fn id(&self) -> &str {
        match self {
            Self::Session(operation) => &operation.id,
            Self::NativeQuota(operation) => &operation.id,
        }
    }
    pub(super) fn expected_digest(&self) -> &str {
        match self {
            Self::Session(operation) => &operation.expected_digest,
            Self::NativeQuota(operation) => &operation.expected_digest,
        }
    }
    pub(super) fn session(&self) -> Option<&Operation> {
        match self {
            Self::Session(operation) => Some(operation),
            Self::NativeQuota(_) => None,
        }
    }
    pub(super) fn valid(&self, schema: u32) -> bool {
        match self {
            Self::Session(operation) => operation.valid(),
            Self::NativeQuota(operation) => {
                schema == 4
                    && valid_hex(&operation.id, 32)
                    && valid_hex(&operation.expected_digest, 64)
                    && operation.credential_generation > 0
                    && operation.native_quota.schema == 1
                    && valid_hex(&operation.native_quota.read_context_digest, 64)
            }
        }
    }
}

#[cfg(test)]
#[path = "codex_grant_refresh_actor_tests.rs"]
mod tests;
