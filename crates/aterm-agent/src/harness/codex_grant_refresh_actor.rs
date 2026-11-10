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
mod tests {
    use super::*;

    fn session() -> Operation {
        Operation {
            id: "a".repeat(32),
            caller: slot::Caller {
                sid: "s-wire-fixture".into(),
                pid: 123,
                birth: 456,
            },
            slot: "b".repeat(64),
            expected_digest: "c".repeat(64),
            assignment_generation: 7,
            source_authority: SourceAuthority::Assignment {
                operation: "d".repeat(32),
            },
            credential_generation: 13,
        }
    }
    #[test]
    fn native_actor_preserves_exact_session_bytes_and_attempt_digest() {
        let original = session();
        let actor = JournalOperation::Session(original.clone());
        let old = aterm_json::to_vec(&original).unwrap();
        assert_eq!(aterm_json::to_vec(&actor).unwrap(), old);
        assert_eq!(
            operation_digest(&original).unwrap(),
            operation_digest(&actor).unwrap()
        );
        let decoded: JournalOperation = aterm_json::from_slice(&old).unwrap();
        assert!(decoded.session().unwrap().caller == original.caller);
        assert!(decoded.valid(3));
    }
    #[test]
    fn native_quota_data_cannot_become_session_or_old_schema_authority() {
        let native = JournalOperation::NativeQuota(NativeQuotaOperation {
            id: "a".repeat(32),
            expected_digest: "c".repeat(64),
            credential_generation: 13,
            native_quota: NativeQuotaContext {
                schema: 1,
                read_context_digest: "e".repeat(64),
            },
        });
        assert!(native.valid(4));
        assert!(!native.valid(3));
        assert!(native.session().is_none());
        let bytes = aterm_json::to_vec(&native).unwrap();
        assert!(aterm_json::from_slice::<Operation>(&bytes).is_err());
        assert_ne!(
            operation_digest(&native).unwrap(),
            operation_digest(&session()).unwrap()
        );
        let mut mixed = aterm_json::to_value(session()).unwrap();
        mixed.as_object_mut().unwrap().insert(
            "native_quota".into(),
            aterm_json::json!({"schema":1,"read_context_digest":"e".repeat(64)}),
        );
        assert!(aterm_json::from_value::<JournalOperation>(mixed).is_err());
    }
}
