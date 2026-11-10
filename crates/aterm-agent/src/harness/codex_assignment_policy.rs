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
mod tests {
    use super::*;
    use aterm_json::json;

    fn document(claims: aterm_json::Value) -> aterm_json::Value {
        let payload = aterm_codec::base64::encode(&aterm_json::to_vec(&claims).unwrap())
            .unwrap()
            .trim_end_matches('=')
            .replace('+', "-")
            .replace('/', "_");
        json!({"tokens":{
            "id_token":format!("header.{payload}.signature"),
            "oauth_provenance":{"issuer":"https://auth.example/","client_id":"client"}
        }})
    }
    fn observe(document: &aterm_json::Value) -> Result<ProviderPolicy, String> {
        ProviderPolicy::from_document(&aterm_json::to_vec(document).unwrap())
    }

    #[test]
    fn policy_binding_uses_native_endpoint_client_and_exact_length_encoding() {
        let conventional = observe(&document(json!({"iss":"https://unrelated.example"}))).unwrap();
        assert_eq!(
            conventional.binding(),
            "3af4df6e85b607c4d04337b7339f46469f0f81a72baf53241892d1d5637d21b4"
        );
        assert_eq!(conventional.endpoint(), "https://auth.example/oauth/token");
        assert!(!conventional.fedramp());
        let federal = observe(&document(json!({"https://api.openai.com/auth":{
            "chatgpt_account_is_fedramp":true
        }})))
        .unwrap();
        assert_eq!(conventional.binding(), federal.binding());
        assert!(federal.fedramp());
        assert_eq!(format!("{federal:?}"), "ProviderPolicy(redacted)");
    }

    #[test]
    fn unknown_provenance_or_malformed_realm_never_gets_a_default_policy() {
        let original = document(json!({}));
        for key in ["oauth_provenance", "id_token"] {
            let mut changed = original.clone();
            changed
                .as_object_mut()
                .unwrap()
                .get_mut("tokens")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(key.into(), aterm_json::Value::Null);
            assert!(observe(&changed).is_err());
        }
        for claims in [
            json!([]),
            json!({"https://api.openai.com/auth":null}),
            json!({"https://api.openai.com/auth":{"chatgpt_account_is_fedramp":"false"}}),
        ] {
            assert!(observe(&document(claims)).is_err());
        }
        let mut changed = original;
        changed
            .as_object_mut()
            .unwrap()
            .get_mut("tokens")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("oauth_provenance")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("issuer".into(), json!("http://auth.example"));
        assert!(observe(&changed).is_err());
    }
}
