// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A credential the controller uses, on the one path: the secrets registry this Mac applied
//! names the want's entry and its value is read in place from this Mac's verified store
//! ([`System::vault_entry`]); else the want is filed in the need ledger
//! ([`aterm_secrets::need::raise`]), which asks the lab's Macs, and the operation that needed
//! it is blocked with a line naming the want and the request. Nothing asks a person, and no
//! file under `~/.secrets` is read.

use crate::port::{Secret, System};
use aterm_secrets::need::{self, Purpose, Want};

/// The largest credential value read.
const MAX: usize = 64 * 1024;

/// `want`'s value through the one path, the store's entry holding one value: `token` reads it as
/// one token (no whitespace), else as the entry's one line.
///
/// # Errors
/// One line: this Mac holds no usable `want`, why, and the need it filed for it (or why none
/// was filed).
pub(crate) fn resolve(
    sys: &dyn System,
    want: &Want,
    purpose: Purpose,
    token: bool,
) -> Result<Secret, String> {
    let Some(home) = sys.account_home() else {
        return Err(format!(
            "this account's home cannot be read, so {} cannot be looked up",
            want.key()
        ));
    };
    let paths = aterm_secrets::client::Paths::locate_for_home(&home);
    let names = want.entries();
    let why = match aterm_secrets::registry::read(&paths) {
        Ok(registry) => match registry.iter().find(|entry| names.contains(&entry.name)) {
            Some(entry) => match read(sys, &home, &entry.name, token) {
                Ok(value) => return Ok(value),
                Err(why) => why,
            },
            None => "the vault registry names none".to_owned(),
        },
        Err(why) => why,
    };
    Err(file_need(&paths, want, purpose, &why))
}

/// One value of the store's entry `name`, read in place: never quoted.
fn read(
    sys: &dyn System,
    home: &std::path::Path,
    name: &str,
    token: bool,
) -> Result<Secret, String> {
    crate::port::no_core_dumps();
    let bytes = sys
        .vault_entry(home, name)
        .map_err(|why| format!("the vault entry {name} cannot be read here ({why})"))?
        .ok_or_else(|| format!("the vault holds no {name} here"))?;
    let value = std::str::from_utf8(bytes.as_slice())
        .ok()
        .filter(|_| bytes.len() <= MAX)
        .map(|text| text.strip_suffix('\n').unwrap_or(text))
        .map(|text| text.strip_suffix('\r').unwrap_or(text))
        .map(|text| if token { text.trim() } else { text })
        .filter(|text| {
            !text.is_empty()
                && !text.contains(char::is_control)
                && !(token && text.contains(char::is_whitespace))
        })
        .map(|text| Secret::new(text.as_bytes()));
    match value {
        Some(Ok(value)) => Ok(value),
        _ => Err(format!("the vault entry {name} holds no single value")),
    }
}

/// File `want` in the need ledger under `purpose` and say so: the line a blocked operation
/// reports.
fn file_need(
    paths: &aterm_secrets::client::Paths,
    want: &Want,
    purpose: Purpose,
    why: &str,
) -> String {
    let key = want.key();
    match need::raise(paths, want.clone(), purpose) {
        Ok(need) => format!(
            "this Mac holds no usable {key} ({why}); asked the lab's Macs (request {})",
            aterm_codec::hex::encode_lower(&need.id)
        ),
        Err(error) => {
            format!("this Mac holds no usable {key} ({why}); the request was not filed: {error}")
        }
    }
}

#[cfg(test)]
#[path = "credential_tests.rs"]
pub(super) mod tests;
