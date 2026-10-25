// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Versioned admin custody. A vault envelope key can rotate without destroying
//! the immutable Commons authority. Only the encrypted admin locker holds keys.

use super::admission::Ledger;
use super::{Document, Error, SecretBytes, envelope};
use crate::commons::{ENTRY, bootstrap::Descriptor};
use zeroize::{Zeroize, Zeroizing};

const LEGACY_MAGIC: &str = "alab-vault-admin/1";
const V2_MAGIC: &str = "alab-vault-admin/2";
const MAGIC: &str = "alab-vault-admin/3";

pub(super) fn admin_plaintext_with_admissions(
    seed: &[u8; 32],
    commons_seed: &[u8; 32],
    commons_bound: bool,
    doc: &Document,
    admissions: &Ledger,
) -> SecretBytes {
    let rendered = doc.render();
    let signer = Zeroizing::new(crate::b64::encode_padded(seed));
    let commons = Zeroizing::new(crate::b64::encode_padded(commons_seed));
    let admissions = Zeroizing::new(crate::b64::encode_padded(&admissions.bytes()));
    // Concatenate once at the exact size: growing a buffer after copying seed
    // text would leave that copy in an unwiped, reallocated heap allocation.
    let parts: [&[u8]; 10] = [
        MAGIC.as_bytes(),
        b"\nsigner-seed: ",
        signer.as_bytes(),
        b"\ncommons-seed: ",
        commons.as_bytes(),
        if commons_bound {
            b"\ncommons-bound: yes"
        } else {
            b"\ncommons-bound: no"
        },
        b"\nadmissions: ",
        admissions.as_bytes(),
        b"\n\n",
        rendered.as_slice(),
    ];
    SecretBytes::new(parts.concat())
}

pub(super) struct AdminText {
    pub(super) seed: [u8; 32],
    pub(super) commons_seed: [u8; 32],
    pub(super) commons_bound: bool,
    pub(super) doc: Document,
    pub(super) admissions: Ledger,
}

impl Drop for AdminText {
    fn drop(&mut self) {
        self.seed.zeroize();
        self.commons_seed.zeroize();
    }
}

fn decode_seed(text: &str) -> crate::Result<Zeroizing<[u8; 32]>> {
    let decoded = Zeroizing::new(crate::b64::decode_padded(text).unwrap_or_default());
    let mut seed = Zeroizing::new([0; 32]);
    if decoded.len() != seed.len() {
        return Err(Error::new("admin locker: bad seed"));
    }
    seed.copy_from_slice(&decoded);
    Ok(seed)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAuthorityCustody",
        action = "LegacyMigration",
        project = "aterm_secrets::client::admin_keys::tests::custody_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAuthorityCustody",
        action = "LostLegacyKey",
        project = "aterm_secrets::client::admin_keys::tests::custody_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAuthorityCustody",
        action = "MissingCustody",
        project = "aterm_secrets::client::admin_keys::tests::custody_transition"
    )
)]
pub(super) fn parse_admin(plain: &SecretBytes) -> crate::Result<AdminText> {
    let text = plain
        .as_str()
        .ok_or_else(|| Error::new("admin locker: not UTF-8"))?;
    let (header, doc) = text
        .split_once("\n\n")
        .ok_or_else(|| Error::new("admin locker: malformed"))?;
    let mut lines = header.lines();
    let magic = lines.next().unwrap_or_default();
    if !matches!(magic, LEGACY_MAGIC | V2_MAGIC | MAGIC) {
        return Err(Error::new("admin locker: unsupported custody version"));
    }
    let signer = lines
        .next()
        .and_then(|line| line.strip_prefix("signer-seed: "))
        .ok_or_else(|| Error::new("admin locker: missing signer custody"))?;
    let seed = decode_seed(signer)?;
    let commons_seed = if magic == LEGACY_MAGIC {
        Zeroizing::new(*seed)
    } else {
        let commons = lines
            .next()
            .and_then(|line| line.strip_prefix("commons-seed: "))
            .ok_or_else(|| Error::new("admin locker: missing Commons custody"))?;
        decode_seed(commons)?
    };
    // A legacy locker cannot attest that no replica has ever pinned its key.
    // Missing historical evidence is not permission to rotate a Commons root.
    let commons_bound = if magic == LEGACY_MAGIC {
        true
    } else {
        match lines.next() {
            Some("commons-bound: yes") => true,
            Some("commons-bound: no") => false,
            _ => return Err(Error::new("admin locker: missing Commons lifetime")),
        }
    };
    let mut admissions = if magic == MAGIC {
        let encoded = lines
            .next()
            .and_then(|line| line.strip_prefix("admissions: "))
            .ok_or_else(|| Error::new("admin locker: missing admissions"))?;
        if encoded.len() > super::admission::MAX_BYTES.div_ceil(3) * 4 {
            return Err(Error::new("admin locker: oversized admissions"));
        }
        let bytes = crate::b64::decode_padded(encoded)
            .ok_or_else(|| Error::new("admin locker: malformed admissions"))?;
        if crate::b64::encode_padded(&bytes) != encoded {
            return Err(Error::new("admin locker: noncanonical admissions"));
        }
        Ledger::decode(&bytes)?
    } else {
        Ledger::default()
    };
    if lines.next().is_some() {
        return Err(Error::new("admin locker: unexpected custody field"));
    }
    let doc = Document::parse(doc).map_err(|e| Error::new(format!("admin locker: {e}")))?;
    if !commons_bound && has_control(&doc) {
        return Err(Error::new(
            "admin locker: unbound Commons key has a control entry",
        ));
    }
    validate_commons(&doc, &commons_seed)?;
    if magic != MAGIC {
        admissions = admissions.retained(&doc)?;
    }
    admissions.validate(&doc, u64::MAX)?;
    Ok(AdminText {
        seed: *seed,
        commons_seed: *commons_seed,
        commons_bound,
        doc,
        admissions,
    })
}

pub(super) fn has_control(doc: &Document) -> bool {
    doc.commons_audience().is_some()
}

/// Opaque pre-bootstrap values declare an audience but pin no typed root. Once
/// a descriptor exists, neither migration nor an edit may silently replace its
/// authority. Public vault handoff signatures cannot reconstruct a lost key.
pub(super) fn validate_commons(doc: &Document, seed: &[u8; 32]) -> crate::Result<()> {
    for entry in doc.entries() {
        if entry.name == ENTRY && entry.value.as_slice().starts_with(b"ACD1") {
            let descriptor = Descriptor::decode(entry.value.as_slice())?;
            if descriptor.trust().authority != envelope::signer_public(seed)? {
                return Err(Error::new(
                    "admin locker: Commons authority custody does not match its pinned descriptor; \
                     restore custody from an admin that retains the original key",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn control_descriptor(doc: &Document) -> crate::Result<Option<Descriptor>> {
    for entry in doc.entries() {
        if entry.name == ENTRY && entry.value.as_slice().starts_with(b"ACD1") {
            return Descriptor::decode(entry.value.as_slice()).map(Some);
        }
    }
    Ok(None)
}

/// A published root is immutable even if a draft hides or replaces its entry.
/// Audience and repair witnesses may change; neither changes the trust anchor.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbFoundationBirth",
        action = "RemoveRoot",
        project = "aterm_secrets::client::birth_tests::birth_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbFoundationBirth",
        action = "ReplaceRoot",
        project = "aterm_secrets::client::birth_tests::birth_transition"
    )
)]
pub(super) fn validate_commons_transition(
    published: &Document,
    draft: &Document,
) -> crate::Result<()> {
    if let Some(before) = control_descriptor(published)? {
        let after = control_descriptor(draft)?;
        if after.as_ref().map(Descriptor::trust) != Some(before.trust()) {
            return Err(Error::new(
                "cannot remove or replace the published Commons root; a Commons authority transfer is required and is not yet supported",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
