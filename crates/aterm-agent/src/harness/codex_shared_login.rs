// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fresh provider verification for an already staged, device-local Codex login.
//! This reader imports no OAuth session and exposes no credential fingerprint.

use super::*;

/// Full provider identity, distinct from the abbreviated account display key.
#[must_use]
pub fn principal(account: &str, user: &str) -> Option<[u8; 32]> {
    if [account, user]
        .iter()
        .any(|word| word.is_empty() || word.len() > 1024 || word.chars().any(char::is_control))
    {
        return None;
    }
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm/codex-provider-principal/1\0");
    for word in [account, user] {
        hash.update((word.len() as u64).to_be_bytes());
        hash.update(word.as_bytes());
    }
    Some(hash.finalize())
}

/// Private evidence of the exact local credential the provider accepted.
pub struct VerifiedLogin {
    home: PathBuf,
    bytes: Secret,
    email: String,
    principal: [u8; 32],
    plan: Option<String>,
}

impl VerifiedLogin {
    #[must_use]
    pub fn email(&self) -> &str {
        &self.email
    }
    #[must_use]
    pub const fn principal(&self) -> [u8; 32] {
        self.principal
    }
    #[must_use]
    pub fn plan(&self) -> Option<&str> {
        self.plan.as_deref()
    }

    /// Bounded local reads only; an edited token or issuer invalidates the proof.
    pub fn check_current(&self) -> Result<(), String> {
        let current = snapshot(&self.home)?;
        if current.expose() != self.bytes.expose() {
            return Err("staged Codex credential changed after provider verification".into());
        }
        Ok(())
    }
}

fn snapshot(home: &Path) -> Result<Secret, String> {
    #[cfg(unix)]
    {
        let mut bytes = vec![0; AUTH_FILE_MAX];
        let read =
            aterm_uds::private_file::read_private_into(home, AUTH_FILE, AUTH_FILE_MAX, &mut bytes)
                .map_err(|_| "staged Codex credential is not private, owned, or intact")?;
        bytes.truncate(read.len);
        Ok(Secret::from_vec(bytes))
    }
    #[cfg(not(unix))]
    {
        let _ = home;
        Err("private shared-login evidence is unavailable on this platform".into())
    }
}

fn identity(bytes: &[u8], expected: &str) -> Result<(String, [u8; 32], Option<String>), String> {
    let doc: Value =
        aterm_json::from_slice(bytes).map_err(|_| "staged Codex credential is not valid JSON")?;
    if doc
        .get("OPENAI_API_KEY")
        .is_some_and(|value| !value.is_null())
    {
        return Err("shared Codex preparation requires device-local OAuth".into());
    }
    if let Some(provenance) = doc["tokens"].get("oauth_provenance")
        && (provenance["issuer"].as_str() != Some(DEFAULT_ISSUER)
            || provenance["client_id"].as_str() != Some(CLIENT_ID))
    {
        return Err("staged Codex login has an unapproved OAuth issuer".into());
    }
    let stored = stored_auth_from_doc(doc)
        .ok_or("staged Codex login is not a supported OAuth credential")?;
    check_stored_identity(&stored, Some(expected))
        .map_err(|_| "staged Codex login names another identity")?;
    let access = stored
        .access_claims
        .as_ref()
        .ok_or("Codex access token has no identity")?;
    let user = access
        .user_id
        .as_deref()
        .ok_or("Codex access token has no user")?;
    let account = access
        .account_id
        .as_deref()
        .ok_or("Codex access token has no account")?;
    if stored
        .account_id
        .as_deref()
        .is_some_and(|other| other != account)
        || stored
            .id_claims
            .as_ref()
            .and_then(|id| id.account_id.as_deref())
            .is_some_and(|other| other != account)
        || stored
            .id_claims
            .as_ref()
            .and_then(|id| id.user_id.as_deref())
            .is_some_and(|other| other != user)
    {
        return Err("Codex token identities disagree".into());
    }
    // The usage endpoint accepts the bearer access token. A separate, locally
    // supplied ID token has no verified signature here and cannot prove email.
    let email = access
        .email
        .as_deref()
        .filter(|email| super::super::accounts::email_matches(email, expected))
        .ok_or("Codex token does not prove the approved email")?;
    let principal = principal(account, user).ok_or("invalid Codex provider identity")?;
    let email = email.to_owned();
    let plan = access.plan.clone();
    Ok((email, principal, plan))
}

/// Local identity is used only to refuse a shared credential on an unapproved path.
/// It is not an authenticated provider observation or permission to publish.
pub(in crate::harness) fn local_principal(home: &Path, expected: &str) -> Option<[u8; 32]> {
    identity(snapshot(home).ok()?.expose(), expected)
        .ok()
        .map(|(_, principal, _)| principal)
}

/// Ask the canonical provider to accept the exact staged credential. A refresh
/// is journaled by the existing reader; changed bytes are observed on the next
/// pass rather than attributed to a request made with a different snapshot.
pub fn verify_in_home(
    home: &Path,
    expected: &str,
    wait: Duration,
    now: i64,
) -> Result<Option<VerifiedLogin>, String> {
    verify_with(home, expected, || {
        read_login_with(
            home,
            &usage_url_from_base(DEFAULT_CHATGPT_BASE_URL),
            &format!("{DEFAULT_ISSUER}/oauth/token"),
            wait,
            now,
            Some(expected),
        )
    })
}

fn verify_with(
    home: &Path,
    expected: &str,
    read: impl FnOnce() -> Reading,
) -> Result<Option<VerifiedLogin>, String> {
    if !auth_path_in_home(home).exists() {
        return Ok(None);
    }
    let bytes = snapshot(home)?;
    let (email, principal, plan) = identity(bytes.expose(), expected)?;
    if !matches!(read(), Reading::Usage(_)) {
        return Err("provider has not verified the staged Codex login".into());
    }
    let proof = VerifiedLogin {
        home: home.to_owned(),
        bytes,
        email,
        principal,
        plan,
    };
    proof.check_current()?;
    Ok(Some(proof))
}

#[cfg(test)]
#[path = "codex_shared_login_tests.rs"]
mod tests;
