// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Policy observations require the actual admitted producer document. They
//! describe it and confer no source, credential, or dispatch permission.

use super::owner;
use super::prepare::OAuth;

/// Immutable metadata captured inside the actual prepared-grant callback.
/// Serialization conveys policy input, never an installation or source permit.
#[derive(Clone, PartialEq, Eq, serde::Serialize)]
pub struct PreparedPreview {
    pub(super) operation: String,
    pub(super) pool_revision: String,
    pub(super) expected_assignment: u64,
    pub(super) target_ref: String,
    pub(super) auth_mode: &'static str,
    pub(super) account: String,
    pub(super) user: String,
    pub(super) source_grant: String,
    pub(super) target_grant: String,
    pub(super) source_provider: String,
    pub(super) target_provider: String,
    pub(super) source_fedramp: bool,
    pub(super) target_fedramp: bool,
    pub(super) target_refresh_endpoint: String,
}

impl std::fmt::Debug for PreparedPreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PreparedPreview(redacted)")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProviderPolicy {
    binding: String,
    endpoint: String,
    fedramp: bool,
}

impl std::fmt::Debug for ProviderPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProviderPolicy(redacted)")
    }
}

impl ProviderPolicy {
    pub fn binding(&self) -> &str {
        &self.binding
    }
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
    pub fn fedramp(&self) -> bool {
        self.fedramp
    }

    pub(in crate::harness) fn capture(grant: &owner::AdmittedGrant<'_>) -> Result<Self, String> {
        Self::from_document(grant.document())
    }

    fn from_document(document: &[u8]) -> Result<Self, String> {
        let oauth = OAuth::from_document(document)?;
        let document: aterm_json::Value =
            aterm_json::from_slice(document).map_err(|_| "policy document unavailable")?;
        let token = document
            .get("tokens")
            .and_then(|v| v.get("id_token"))
            .and_then(aterm_json::Value::as_str)
            .ok_or("policy realm unavailable")?;
        let mut parts = token.split('.');
        let header = parts.next().ok_or("policy realm unavailable")?;
        let payload = parts.next().ok_or("policy realm unavailable")?;
        let signature = parts.next().ok_or("policy realm unavailable")?;
        if header.is_empty() || payload.is_empty() || signature.is_empty() || parts.next().is_some()
        {
            return Err("policy realm unavailable".into());
        }
        let mut encoded = payload.replace('-', "+").replace('_', "/");
        while encoded.len() % 4 != 0 {
            encoded.push('=');
        }
        let bytes = zeroize::Zeroizing::new(
            aterm_codec::base64::decode(&encoded).map_err(|_| "policy realm unavailable")?,
        );
        let claims: aterm_json::Value =
            aterm_json::from_slice(&bytes).map_err(|_| "policy realm unavailable")?;
        if !claims.is_object() {
            return Err("policy realm unavailable".into());
        }
        // Match Codex's actual ID-token realm interpretation. A present
        // malformed classification is unknown, never a conventional account.
        let fedramp = match claims.get("https://api.openai.com/auth") {
            None => false,
            Some(auth) if auth.is_object() => match auth.get("chatgpt_account_is_fedramp") {
                None => false,
                Some(value) => value.as_bool().ok_or("policy realm unavailable")?,
            },
            Some(_) => return Err("policy realm unavailable".into()),
        };
        let mut hash = aterm_digest::Sha256::new();
        hash.update(b"aterm/codex-assignment-provider/v1\0");
        for field in [&oauth.token_endpoint, &oauth.client] {
            let length = u32::try_from(field.len()).map_err(|_| "policy provider unavailable")?;
            hash.update(length.to_be_bytes());
            hash.update(field.as_bytes());
        }
        Ok(Self {
            binding: aterm_codec::hex::encode_lower(&hash.finalize()),
            endpoint: oauth.token_endpoint,
            fedramp,
        })
    }
}

#[cfg(test)]
#[path = "codex_assignment_policy_tests.rs"]
mod tests;
