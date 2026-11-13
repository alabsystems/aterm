// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Validators for the compiled want catalog; refusals never contain the submitted bytes.

use super::*;
use aterm_secrets::need::transfer::Values;
use zeroize::{Zeroize, Zeroizing};

struct WipedJson(Value);
impl Drop for WipedJson {
    fn drop(&mut self) {
        fn wipe(value: &mut Value) {
            match value {
                Value::String(text) => text.zeroize(),
                Value::Array(items) => items.iter_mut().for_each(wipe),
                Value::Object(fields) => fields.values_mut().for_each(wipe),
                _ => {}
            }
        }
        wipe(&mut self.0);
    }
}

pub(super) fn is_line(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok_and(|text| {
        let text = text.strip_suffix('\n').unwrap_or(text);
        let text = text.strip_suffix('\r').unwrap_or(text);
        !text.trim().is_empty() && !text.chars().any(char::is_control)
    })
}

fn line(bytes: &[u8]) -> bool {
    is_line(bytes)
}

/// Validate all entries and return the deployer's public account identity when applicable.
pub(super) fn validate(want: &Want, values: &Values) -> Result<Option<String>> {
    values.encode().map_err(crate::table::failure)?;
    let entries = want.entries();
    if !want.posted()
        || values.0.first().map(|(name, _)| name) != entries.first()
        || values.0.iter().any(|(name, _)| !entries.contains(name))
        || (!matches!(want, Want::Mailbox(_)) && values.0.len() != 1)
    {
        return Err(Error::new(
            "the contribution does not match the requested want",
        ));
    }
    let refused = || Error::new("the contribution failed the want's validator");
    let bytes = values.0[0].1.as_slice();
    match want {
        Want::Mailbox(_) | Want::MailboxComponent(_) => {
            if values.0.iter().any(|(_, value)| !line(value.as_slice())) {
                return Err(refused());
            }
        }
        Want::Gh(_) => {
            if !line(bytes) {
                return Err(refused());
            }
            let text = std::str::from_utf8(bytes)
                .map_err(|_| refused())?
                .trim_end_matches(['\r', '\n']);
            if text.len() < 20
                || !["ghp_", "github_pat_", "gho_", "ghu_", "ghs_", "ghr_"]
                    .iter()
                    .any(|prefix| text.starts_with(prefix))
                || !text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                return Err(refused());
            }
        }
        Want::Notary => aterm_secrets::client::custodian::validate_notary_contribution(bytes)
            .map_err(crate::table::failure)?,
        Want::SiteDeployerKey => {
            let json = WipedJson(aterm_json::from_slice::<Value>(bytes).map_err(|_| refused())?);
            let get = |name: &str| {
                json.0
                    .get(name)
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
            };
            if get("type") != Some("service_account") || get("token_uri").is_none() {
                return Err(refused());
            }
            let email = get("client_email")
                .filter(|text| text.len() <= 160 && !text.chars().any(char::is_control))
                .ok_or_else(refused)?;
            let pem = get("private_key").ok_or_else(refused)?;
            let body = pem
                .trim()
                .strip_prefix(concat!("-----BEGIN ", "PRIVATE KEY-----"))
                .and_then(|rest| {
                    rest.trim_end()
                        .strip_suffix(concat!("-----END ", "PRIVATE KEY-----"))
                })
                .ok_or_else(refused)?;
            let mut joined = Zeroizing::new(String::with_capacity(body.len()));
            for ch in body.chars() {
                if !ch.is_ascii_whitespace() {
                    joined.push(ch);
                }
            }
            let der = Zeroizing::new(aterm_codec::base64::decode(&joined).map_err(|_| refused())?);
            ring::signature::RsaKeyPair::from_pkcs8(&der).map_err(|_| refused())?;
            return Ok(Some(email.to_owned()));
        }
        _ => return Err(refused()),
    }
    Ok(None)
}

#[cfg(test)]
#[path = "value_tests.rs"]
mod tests;
