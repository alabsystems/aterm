// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Azure service principal Lane A signs as. Its one source is this host's vault: the
//! registry's `azure_signing_principal` entry (`aterm_secrets::need::Want::AzureSigningPrincipal`),
//! read in place from the verified store (`aterm_secrets::client::read_in_place`); no file under
//! the home is opened. A host that holds no usable one files the want with the lab
//! (`aterm_secrets::need::raise`) and signs nothing. The value reaches signtool's environment
//! alone, never a command line, a file winsign writes or a line it prints.

use std::fmt;

/// The principal's variables for signtool's plug-in, `(name, value)`: `AZURE_TENANT_ID`,
/// `AZURE_CLIENT_ID` and `AZURE_CLIENT_SECRET`. Its `Debug` names them, never a value.
pub struct Principal(Vec<(String, String)>);

impl Principal {
    /// A principal of these variables.
    #[must_use]
    pub fn new(vars: Vec<(String, String)>) -> Self {
        Self(vars)
    }

    /// Its variables, for the child's environment.
    #[must_use]
    pub fn env(&self) -> &[(String, String)] {
        &self.0
    }
}

impl fmt::Debug for Principal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|(name, _)| name))
            .finish()
    }
}

/// What a host that holds no usable principal does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Miss {
    /// File the want with the lab and say which request (`sign`).
    File,
    /// Say only why (`doctor`).
    Report,
}

/// The principal from this host's vault.
///
/// # Errors
/// Why there is none: the registry cannot be read, it names no entry, or the store holds no
/// readable or well-formed value for it; with [`Miss::File`], also the request filed for it.
#[cfg(any(unix, windows))]
pub fn from_vault(miss: Miss) -> Result<Principal, String> {
    let paths = aterm_secrets::client::Paths::locate().map_err(|e| e.to_string())?;
    from_vault_in(&paths, miss)
}

/// [`from_vault`] for the vault under `paths`.
#[cfg(any(unix, windows))]
fn from_vault_in(paths: &aterm_secrets::client::Paths, miss: Miss) -> Result<Principal, String> {
    use aterm_secrets::need::catalog::AZURE_SIGNING_PRINCIPAL_ENTRY as ENTRY;
    use aterm_secrets::need::{Purpose, Want};
    let registry = aterm_secrets::registry::read(paths)?;
    let why = if registry.iter().any(|entry| entry.name == ENTRY) {
        match held(paths, ENTRY) {
            Ok(principal) => return Ok(principal),
            Err(why) => why,
        }
    } else {
        format!("this host's vault holds no {ENTRY} entry")
    };
    if miss == Miss::Report {
        return Err(why);
    }
    let want = Want::AzureSigningPrincipal;
    let key = want.key();
    Err(
        match aterm_secrets::need::raise(paths, want, Purpose::WinsignAzure) {
            Ok(need) => format!(
                "{why}; asked the lab's Macs for {key} (request {})",
                aterm_codec::hex::encode_lower(&need.id)
            ),
            Err(e) => format!("{why}; the request for {key} was not filed: {e}"),
        },
    )
}

/// The principal in the store's entry `name`, read in place.
#[cfg(any(unix, windows))]
fn held(paths: &aterm_secrets::client::Paths, name: &str) -> Result<Principal, String> {
    let bytes = aterm_secrets::client::read_in_place(paths, name)
        .map_err(|e| format!("the vault entry {name}: {e}"))?
        .ok_or_else(|| format!("the vault entry {name} is not on this host yet"))?;
    let text = bytes
        .as_str()
        .ok_or_else(|| format!("the vault entry {name} is not UTF-8"))?;
    let vars = aterm_secrets::azure_principal::parse(text)
        .map_err(|why| format!("the vault entry {name}: {why}"))?;
    Ok(Principal::new(
        vars.iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
    ))
}

/// No vault reaches this host: the vault client, its registry and need ledger build on Unix and
/// Windows only.
///
/// # Errors
/// Always.
#[cfg(not(any(unix, windows)))]
pub fn from_vault(_: Miss) -> Result<Principal, String> {
    Err(
        "no vault on this host (the secrets registry and need ledger build on Unix and Windows \
         only), so it holds no Azure signing principal"
            .to_owned(),
    )
}

#[cfg(test)]
#[path = "principal_tests.rs"]
mod tests;
