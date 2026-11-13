// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The original producer's signed publication, joined to the current canonical
//! binding and its physical credential directory. Alias inventory is not this
//! owner's credential-use authority.
use super::*;
use crate::harness::{accounts, codex_native_row};

pub(super) struct PublishedSource {
    issuer: issuer_observation::IssuerObservation,
    receipt: codex_native_row::PublicationReceiptData,
}
impl PublishedSource {
    pub(super) fn authenticate(
        env: &Env,
        binding: &GrantBinding,
        logins: &accounts::Logins,
        auth: &Path,
    ) -> Result<Option<Self>, String> {
        let required = logins
            .enrollment
            .requires_global_catalogue(&binding.request)?;
        if catalogue::existing_scope(env, &binding.request.id)?.is_none() {
            if required {
                return Err("owned source catalogue authority missing".into());
            }
            // Historical custody remains useful for no-HTTP settlement. It is
            // not admitted to native consumer/provider roles by this branch.
            return Ok(None);
        }
        if !required {
            return Err("owned source catalogue admission changed".into());
        }
        let receipt = logins
            .enrollment
            .row_publication(binding)
            .ok_or("owned source publication proof absent")?
            .clone();
        let identity =
            platform::machine_identity(env.home.as_deref().ok_or("owned source home absent")?)
                .map_err(|_| "owned publication identity unavailable")?
                .ok_or("owned publication identity unavailable")?;
        // Authenticate metadata and its physical destination before decrypting
        // an issuer/current document or opening a destination AuthLock.
        issuer_observation::verify_publication_mac(&identity, &receipt)?;
        check_binding(&receipt, logins)?;
        check_location(&receipt, auth)?;
        let issuer =
            issuer_observation::IssuerObservation::read(env, &binding.request, &binding.reference)?;
        issuer.verify_publication(&receipt)?;
        let source = Self { issuer, receipt };
        source.at(logins, auth)?;
        Ok(Some(source))
    }
    pub(super) fn binding_current(&self, logins: &accounts::Logins) -> Result<(), String> {
        check_binding(&self.receipt, logins)?;
        self.issuer.current()
    }
    pub(super) fn at(&self, logins: &accounts::Logins, auth: &Path) -> Result<(), String> {
        self.binding_current(logins)?;
        check_location(&self.receipt, auth)?;
        // A separate owned materialization needs its own typed scope. Path
        // equality, a copied document or a marker cannot create such a scope.
        self.issuer.current()
    }
}
fn check_binding(
    receipt: &codex_native_row::PublicationReceiptData,
    logins: &accounts::Logins,
) -> Result<(), String> {
    if logins.enrollment.row_publication(receipt.binding()) != Some(receipt)
        || !logins
            .enrollment
            .authenticates_published_grant(receipt.binding())
        || receipt.binding_fingerprint()
            != codex_native_row::binding_digest(logins, &receipt.binding().request.label)?
        || !receipt.staging().directories_current()
    {
        return Err("owned source publication binding changed".into());
    }
    Ok(())
}
fn check_location(
    receipt: &codex_native_row::PublicationReceiptData,
    auth: &Path,
) -> Result<(), String> {
    let parent = auth.parent().ok_or("owned source directory absent")?;
    if auth.file_name().is_none_or(|name| name != "auth.json")
        || receipt
            .staging()
            .directory_identity
            .as_ref()
            .is_none_or(|identity| !identity.matches(parent))
    {
        return Err("credential path is outside the authenticated source scope".into());
    }
    Ok(())
}

#[cfg(all(test, unix))]
#[path = "codex_grant_published_source_tests.rs"]
mod tests;
