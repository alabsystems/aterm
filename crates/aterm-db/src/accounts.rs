// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! First database application: atomic account configuration proposals. Credential
//! material stays in `aterm secrets`; this collection accepts references only.
//! A catalog proposal is not proof of a local vendor login or paid-spend authority.

use crate::{Error, Result, crypto::name};
use aterm_json::Value;

#[cfg(unix)]
pub mod proposals;

/// Validate the closed account schema, rejecting unknown credential-bearing fields.
pub fn validate(value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::new("account must be an object"))?;
    let fields = [
        "provider",
        "email",
        "method",
        "realm",
        "plan",
        "secret_ref",
        "machines",
        "label",
        "machine_id",
        "slot",
        "identities",
        "auth",
        "details",
        "source",
    ];
    if object.keys().any(|key| !fields.contains(&key.as_str())) {
        return Err(Error::new(
            "unknown account field; credentials belong in the secrets manager",
        ));
    }
    let provider = value
        .get("provider")
        .and_then(Value::as_str)
        .filter(|word| name(word));
    if provider.is_none() {
        return Err(Error::new("account needs a provider identifier"));
    }
    if !value
        .get("method")
        .and_then(Value::as_str)
        .is_some_and(crate::contacts::method)
    {
        return Err(Error::new("account needs a known authentication method"));
    }
    if let Some(email) = value.get("email").filter(|v| !v.is_null()) {
        let email = email
            .as_str()
            .ok_or_else(|| Error::new("account email must be a string"))?;
        let mut pieces = email.split('@');
        let first = pieces.next().unwrap_or("");
        let second = pieces.next().unwrap_or("");
        if email.len() > 254
            || first.is_empty()
            || second.is_empty()
            || pieces.next().is_some()
            || email.chars().any(char::is_whitespace)
            || email.chars().any(char::is_control)
        {
            return Err(Error::new("account email is invalid"));
        }
    }
    for field in ["realm", "plan"] {
        if let Some(v) = value.get(field).filter(|v| !v.is_null())
            && !v.as_str().is_some_and(name)
        {
            return Err(Error::new("account realm or plan is invalid"));
        }
    }
    if let Some(reference) = value.get("secret_ref").filter(|v| !v.is_null()) {
        crate::contacts::validate_secret_ref(reference)?;
    }
    if let Some(machines) = value.get("machines") {
        let machines = machines.as_array().ok_or_else(|| {
            Error::new("machine priorities must be an ordered list of machine identifiers")
        })?;
        if machines.len() > 64 {
            return Err(Error::new("too many preferred machines"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for machine in machines {
            let machine = machine
                .as_str()
                .filter(|word| name(word))
                .ok_or_else(|| Error::new("invalid preferred machine"))?;
            if !seen.insert(machine) {
                return Err(Error::new("preferred machine occurs twice"));
            }
        }
    }
    crate::contacts::validate_account(value)
}
