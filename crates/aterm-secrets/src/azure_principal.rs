// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Azure signing principal's vault value (`need::catalog::AZURE_SIGNING_PRINCIPAL_ENTRY`):
//! one `NAME=value` line for each of [`VARS`], each once, and nothing else but blank lines. The
//! custodian holds a contribution to it and `aterm-winsign` reads it with [`parse`]; a refusal
//! never holds a value.

/// The variables the value holds, in the order [`parse`] returns them: the service principal
/// Azure's `EnvironmentCredential` authenticates.
pub const VARS: [&str; 3] = ["AZURE_TENANT_ID", "AZURE_CLIENT_ID", "AZURE_CLIENT_SECRET"];

/// The value's three variables, `(name, value)` in [`VARS`] order, borrowed from `text`.
///
/// # Errors
/// A non-blank line that is not `NAME=value` for one of [`VARS`], a value that is empty or
/// holds anything but printable ASCII, a variable given twice, or one missing.
pub fn parse(text: &str) -> Result<[(&'static str, &str); 3], String> {
    let mut found: [Option<&str>; 3] = [None; 3];
    for (n, line) in text.lines().enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() {
            continue;
        }
        let Some((i, value)) = line
            .split_once('=')
            .and_then(|(name, value)| VARS.iter().position(|var| *var == name).map(|i| (i, value)))
        else {
            return Err(format!(
                "line {} is not AZURE_TENANT_ID=, AZURE_CLIENT_ID= or AZURE_CLIENT_SECRET=",
                n + 1
            ));
        };
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(format!(
                "{}'s value is empty or holds a space, a control or a non-ASCII character",
                VARS[i]
            ));
        }
        if found[i].replace(value).is_some() {
            return Err(format!("{} is given twice", VARS[i]));
        }
    }
    let mut out = [("", ""); 3];
    for (i, var) in VARS.iter().enumerate() {
        let value = found[i].ok_or_else(|| format!("no {var} line"))?;
        out[i] = (*var, value);
    }
    Ok(out)
}

#[cfg(test)]
#[path = "azure_principal_tests.rs"]
mod tests;
