// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Nonsecret diagnostics from an already authenticated cached admin snapshot.

use super::{Admin, admin_keys};
use crate::{Result, commons::journal::Trust};

/// Published control metadata, not a current roster or permission to create a
/// network. Absence cannot establish that this authority was never used before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommonsControlInspection {
    pub vault_version: u64,
    pub authority: [u8; 32],
    pub lifetime_bound: bool,
    pub entry_present: bool,
    pub descriptor: Option<Trust>,
}

impl Admin {
    /// Inspect only the authenticated published document, ignoring mutable
    /// drafts. This writes no state, prepares no keys and signs no records.
    pub fn inspect_commons_control(&self) -> Result<CommonsControlInspection> {
        Ok(CommonsControlInspection {
            vault_version: self.seq(),
            authority: self.commons_authority()?,
            lifetime_bound: self.commons_bound,
            entry_present: admin_keys::has_control(&self.published_doc),
            descriptor: admin_keys::control_descriptor(&self.published_doc)?
                .map(|descriptor| descriptor.trust()),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        client::{self, Paths},
        text::{Audience, Document},
    };

    #[test]
    fn control_inspection_uses_published_state_and_exposes_no_payload() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("owner"));
        client::create(
            &paths,
            root.path().join("vault").to_str().unwrap(),
            None,
            Some("m1"),
        )
        .unwrap();
        let mut admin = client::admin_cached(&paths).unwrap();
        let before = admin.inspect_commons_control().unwrap();
        assert!(before.entry_present && before.lifetime_bound);
        assert_eq!(before.descriptor.unwrap().authority, before.authority);
        admin.doc = Document::new_vault(
            "draft",
            &client::machine_identity(&paths)
                .unwrap()
                .recipient()
                .to_bech32(),
        );
        admin
            .doc
            .set_entry(
                &Audience::Everyone,
                "private-payload",
                b"do-not-expose-this-value",
            )
            .unwrap();
        let after = admin.inspect_commons_control().unwrap();
        assert_eq!(before, after);
        assert!(!format!("{after:?}").contains("do-not-expose-this-value"));
    }

    #[test]
    fn control_inspection_distinguishes_absent_and_opaque_without_initializing() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("owner"));
        client::test_create_unbound(
            &paths,
            root.path().join("vault").to_str().unwrap(),
            None,
            Some("m1"),
        )
        .unwrap();
        let admin = client::admin_cached(&paths).unwrap();
        let absent = admin.inspect_commons_control().unwrap();
        assert!(!absent.entry_present && absent.descriptor.is_none());
        assert!(!paths.dir.join("commons").exists());
        let mut doc = admin.doc.clone();
        doc.set_entry(
            &Audience::Hosts(vec!["m1".into()]),
            crate::commons::ENTRY,
            b"opaque-legacy-control",
        )
        .unwrap();
        client::admin_publish(&paths, &admin, &doc).unwrap();
        let opaque = client::admin_cached(&paths)
            .unwrap()
            .inspect_commons_control()
            .unwrap();
        assert!(opaque.entry_present && opaque.descriptor.is_none());
        assert!(!paths.dir.join("commons").exists());
    }
}
