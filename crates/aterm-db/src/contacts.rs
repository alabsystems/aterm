// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Mailboxes, distinct alias addresses, phone identities and provider-specific
//! authentication routes. Correlation metadata may replicate; bearer links may not.
use crate::{Error, Result, crypto::name};
use aterm_json::Value;

/// Known route types. Providers choose among these using typed adapter workflows.
pub fn method(value: &str) -> bool {
    matches!(
        value,
        "oauth" | "device" | "api-key" | "browser" | "cli" | "email-link" | "phone" | "passkey"
    )
}
/// A bounded map of explicitly classified extra information. Credential fields
/// and payment instruments must be references to the secrets manager.
pub fn validate_details(value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .filter(|o| o.len() <= 64)
        .ok_or_else(|| Error::new("details must be a bounded object"))?;
    for (key, detail) in object {
        if !name(key) {
            return Err(Error::new("invalid detail name"));
        }
        let d = detail
            .as_object()
            .filter(|d| d.len() == 2)
            .ok_or_else(|| Error::new("each detail needs kind and value"))?;
        let kind = d.get("kind").and_then(Value::as_str);
        let text = d
            .get("value")
            .and_then(Value::as_str)
            .filter(|s| s.len() <= 2048 && !s.chars().any(char::is_control));
        if !matches!(
            kind,
            Some(
                "note"
                    | "status"
                    | "requirement"
                    | "secret-ref"
                    | "phone-ref"
                    | "email-ref"
                    | "source-ref"
            )
        ) || text.is_none()
        {
            return Err(Error::new(
                "detail must be classified as non-secret text or a reference",
            ));
        }
        if matches!(
            kind,
            Some("secret-ref" | "phone-ref" | "email-ref" | "source-ref")
        ) && !text.is_some_and(|s| s.split(':').all(name))
        {
            return Err(Error::new(
                "reference details must name objects, never bearer links or credential values",
            ));
        }
        if [
            "password",
            "token",
            "cookie",
            "recovery-code",
            "credit-card",
            "private-key",
        ]
        .iter()
        .any(|word| key.to_ascii_lowercase().contains(word))
            && kind != Some("secret-ref")
        {
            return Err(Error::new("credential detail must be a secret reference"));
        }
    }
    Ok(())
}
/// Check optional structured fields extending the common account schema.
pub fn validate_account(value: &Value) -> Result<()> {
    if let Some(label) = value.get("label")
        && !label.as_str().is_some_and(name)
    {
        return Err(Error::new("invalid account display label"));
    }
    if let Some(machine) = value.get("machine_id")
        && !machine
            .as_str()
            .is_some_and(|id| crate::database::decode_hex::<16>(id).is_ok())
    {
        return Err(Error::new(
            "account must reference a stable machine identifier",
        ));
    }
    if let Some(slot) = value.get("slot")
        && !slot
            .as_u64()
            .is_some_and(|n| n > 0 && n <= u64::from(u32::MAX))
    {
        return Err(Error::new("account slot must be a positive number"));
    }
    if let Some(identities) = value.get("identities") {
        let refs = identities
            .as_array()
            .filter(|v| v.len() <= 32)
            .ok_or_else(|| Error::new("identities must be a bounded contact-reference list"))?;
        for identity in refs {
            let o = identity
                .as_object()
                .filter(|o| o.len() == 2)
                .ok_or_else(|| Error::new("identity reference needs kind and id"))?;
            if !matches!(
                o.get("kind").and_then(Value::as_str),
                Some("email" | "phone")
            ) || !o.get("id").and_then(Value::as_str).is_some_and(name)
            {
                return Err(Error::new("invalid contact identity reference"));
            }
        }
    }
    if let Some(auth) = value.get("auth") {
        validate_auth(auth)?;
    }
    for key in ["details", "source"] {
        if let Some(details) = value.get(key) {
            validate_details(details)?;
        }
    }
    Ok(())
}
/// Provider workflows can prefer email links and name required additional challenges.
pub fn validate_auth(value: &Value) -> Result<()> {
    let routes = value
        .as_array()
        .filter(|v| v.len() <= 16)
        .ok_or_else(|| Error::new("authentication routes must be a bounded list"))?;
    for route in routes {
        let o = route
            .as_object()
            .ok_or_else(|| Error::new("authentication route must be an object"))?;
        if o.keys().any(|k| {
            !["kind", "identity", "mailbox", "requirements", "secret_ref"].contains(&k.as_str())
        }) {
            return Err(Error::new("unknown authentication route field"));
        }
        if !o.get("kind").and_then(Value::as_str).is_some_and(method) {
            return Err(Error::new("unknown authentication route"));
        }
        for key in ["identity", "mailbox", "secret_ref"] {
            if let Some(v) = o.get(key)
                && !v.as_str().is_some_and(name)
            {
                return Err(Error::new("authentication references must be identifiers"));
            }
        }
        if let Some(requirements) = o.get("requirements") {
            let r = requirements
                .as_array()
                .filter(|r| r.len() <= 16)
                .ok_or_else(|| Error::new("authentication requirements must be a list"))?;
            if !r.iter().all(|v| {
                matches!(
                    v.as_str(),
                    Some(
                        "email-access"
                            | "sms"
                            | "authenticator"
                            | "passkey"
                            | "recovery"
                            | "human-approval"
                            | "browser-profile"
                    )
                )
            }) {
                return Err(Error::new("unknown authentication requirement"));
            }
        }
    }
    Ok(())
}
/// Validate a contact entity. Alias links are explicit; no email normalization occurs.
pub fn validate(collection: &str, value: &Value) -> Result<()> {
    let o = value
        .as_object()
        .ok_or_else(|| Error::new("contact must be an object"))?;
    let allowed = match collection {
        "mailboxes" => &["name", "provider", "auth", "secret_ref", "details"][..],
        "email-addresses" => &["address", "mailbox_id", "alias_of", "details"][..],
        "phones" => &["number", "label", "details"][..],
        _ => return Err(Error::new("unknown contact collection")),
    };
    if o.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(Error::new("unknown contact field"));
    }
    if collection == "email-addresses" {
        let address = o
            .get("address")
            .and_then(Value::as_str)
            .filter(|s| {
                s.len() <= 254 && !s.chars().any(char::is_control) && s.split('@').count() == 2
            })
            .ok_or_else(|| Error::new("invalid email address"))?;
        if address.starts_with('@')
            || address.ends_with('@')
            || address.chars().any(char::is_whitespace)
        {
            return Err(Error::new("invalid email address"));
        }
        for k in ["mailbox_id", "alias_of"] {
            if let Some(v) = o.get(k)
                && !v.as_str().is_some_and(name)
            {
                return Err(Error::new(
                    "mailbox and alias links must be explicit identifiers",
                ));
            }
        }
    }
    if collection == "mailboxes" {
        if !o.get("name").and_then(Value::as_str).is_some_and(name) {
            return Err(Error::new("mailbox needs a stable display name"));
        }
        if let Some(auth) = o.get("auth") {
            validate_auth(auth)?;
        }
    }
    if collection == "phones"
        && let Some(number) = o.get("number")
    {
        let n = number
            .as_str()
            .ok_or_else(|| Error::new("phone must be an international number"))?;
        if !n.starts_with('+')
            || !(8..=16).contains(&n.len())
            || !n[1..].bytes().all(|b| b.is_ascii_digit())
        {
            return Err(Error::new("phone needs international +country-code form"));
        }
    }
    if let Some(details) = o.get("details") {
        validate_details(details)?;
    }
    Ok(())
}
