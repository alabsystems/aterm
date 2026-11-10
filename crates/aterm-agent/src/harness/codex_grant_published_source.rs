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
mod tests {
    use super::*;

    #[test]
    fn identical_credentials_and_same_generation_cannot_retarget_owned_source() {
        let fixture =
            crate::harness::codex_enrollment::NativeRowRecoveryFixture::new(&"b".repeat(64));
        fixture.retire_originals().unwrap();
        let applied = fixture.publish_first().unwrap();
        assert_eq!(applied.phase, codex_enrollment::Phase::Applied);
        let env = fixture.env();
        let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
        let (logins, warning) = store.load_logins(env.now).unwrap();
        assert!(warning.is_none());
        let request = &fixture.requests()[0];
        let binding = logins
            .enrollment
            .published_grant(&request.label, applied.generation.unwrap())
            .unwrap();
        let staging = logins.enrollment.catalogue_staging(request).unwrap();
        let original_auth = std::fs::read(staging.directory.join("auth.json")).unwrap();
        let original_logins = std::fs::read(store.logins_file()).unwrap();
        let copied = env.state.join("copied-source-is-not-authority");
        platform::ensure_private_dir(&copied).unwrap();
        platform::write_private(&copied, "auth.json", &original_auth).unwrap();
        platform::write_private(
            &copied,
            "auth.json.aterm-owner",
            b"ATERM-CODEX-AUTH-OWNER 1\n",
        )
        .unwrap();
        let lifetime = crate::harness::account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        for rewrite_fingerprint in [false, true] {
            let mut changed = accounts::Logins::from_json(
                std::str::from_utf8(&original_logins).unwrap(),
                env.now,
            )
            .unwrap();
            changed.labels.get_mut(&request.label).unwrap().dir =
                Some(copied.to_str().unwrap().into());
            assert!(changed.enrollment.authenticates_published_grant(&binding));
            let fingerprint =
                crate::harness::codex_native_row::binding_digest(&changed, &request.label).unwrap();
            let mut value: aterm_json::Value = aterm_json::from_str(&changed.to_json()).unwrap();
            if rewrite_fingerprint {
                value
                    .as_object_mut()
                    .and_then(|object| object.get_mut("enrollment"))
                    .and_then(aterm_json::Value::as_object_mut)
                    .and_then(|book| book.get_mut(request.id.as_str()))
                    .and_then(aterm_json::Value::as_object_mut)
                    .and_then(|record| record.get_mut("row_publication"))
                    .and_then(aterm_json::Value::as_object_mut)
                    .unwrap()
                    .insert("binding_digest".into(), fingerprint.into());
            }
            // Deliberate hostile canonical metadata substitution, not a writer
            // authority or an alternate publication. The issuer MAC is intact.
            platform::write_private(
                &store.dir,
                accounts::LOGINS_FILE,
                &aterm_json::to_vec(&value).unwrap(),
            )
            .unwrap();
            let (reloaded, warning) = store.load_logins(env.now).unwrap();
            assert!(warning.is_none());
            assert!(reloaded.enrollment.authenticates_published_grant(&binding));
            let mut entered = false;
            assert!(
                with_maintenance_owner(env, &binding, &scope, |_| {
                    entered = true;
                    Ok(())
                })
                .is_err()
            );
            assert!(!entered);
            assert!(
                !copied.join(codex_app_login::AUTH_LOCK_FILE).exists(),
                "unauthenticated destination must be refused before credential custody opens"
            );
            assert!(catalogue::inventory_current_for_test(env).is_err());
            assert_eq!(
                std::fs::read(copied.join("auth.json")).unwrap(),
                original_auth
            );
            assert_eq!(
                std::fs::read(staging.directory.join("auth.json")).unwrap(),
                original_auth
            );
        }
        platform::write_private(&store.dir, accounts::LOGINS_FILE, &original_logins).unwrap();
        assert!(with_maintenance_owner(env, &binding, &scope, |_| Ok(())).is_ok());
        assert!(catalogue::inventory_current_for_test(env).is_ok());
    }
}
