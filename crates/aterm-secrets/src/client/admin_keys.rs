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
/// The admin locker format this build writes.
pub(super) const MAGIC: &str = "alab-vault-admin/3";

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
    /// The format the locker was read in: [`MAGIC`] or an older one a publish rewrites.
    pub(super) custody: &'static str,
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
    let magic = match lines.next().unwrap_or_default() {
        LEGACY_MAGIC => LEGACY_MAGIC,
        V2_MAGIC => V2_MAGIC,
        MAGIC => MAGIC,
        other => return Err(unsupported(other)),
    };
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
    // The marker governs the root key's lifetime (rotation keeps it, demotion
    // refuses, a descriptor must carry it); it is not evidence that Commons
    // control exists, and member removal follows retirement::holds_control.
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
        custody: magic,
        seed: *seed,
        commons_seed: *commons_seed,
        commons_bound,
        doc,
        admissions,
    })
}

/// The refusal of a locker whose first line is `magic`: a later format names itself, so an
/// approver an older build runs says what to update.
fn unsupported(magic: &str) -> Error {
    let number = |text: &str| {
        text.strip_prefix("alab-vault-admin/")
            .filter(|number| (1..=6).contains(&number.len()))
            .and_then(|number| number.parse::<u32>().ok())
    };
    match (number(magic), number(MAGIC)) {
        (Some(found), Some(written)) if found > written => Error::new(format!(
            "admin locker: {magic} is newer than this aterm reads ({MAGIC} at most); run a newer aterm"
        )),
        _ => Error::new("admin locker: unsupported custody version"),
    }
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

/// Test-only stand-ins for a vault an admin before Commons custody (version-1 lockers) wrote.
#[cfg(test)]
pub(super) mod legacy {
    use super::{LEGACY_MAGIC, SecretBytes, Zeroizing, envelope};
    use crate::{
        age,
        client::{self, Admin, Paths},
        envelope::Unsigned,
        transport,
    };

    fn legacy_plaintext(admin: &Admin) -> SecretBytes {
        let seed = Zeroizing::new(crate::b64::encode_padded(&admin.seed));
        let mut bytes = format!("{LEGACY_MAGIC}\nsigner-seed: {}\n\n", seed.as_str()).into_bytes();
        bytes.extend_from_slice(admin.doc.render().as_slice());
        SecretBytes::new(bytes)
    }

    /// Emulate an old admin's complete signed publication, including loss of the
    /// independent Commons field. This is deliberately unavailable in production.
    pub(in crate::client) fn publish_legacy(paths: &Paths, admin: &Admin) {
        let bytes = client::seal(
            &admin.config.name,
            admin.seq() + 1,
            &admin.seed,
            &admin.commons_seed,
            admin.commons_bound,
            &admin.doc,
            &admin.handoff,
        )
        .unwrap();
        let parsed = envelope::parse(&bytes).unwrap();
        let mut lockers = parsed.lockers.clone();
        let recipients = client::recipients_of(&admin.doc.admins(), &admin.doc).unwrap();
        lockers[0].armored =
            age::armor(&age::encrypt(&recipients, legacy_plaintext(admin).as_slice()).unwrap());
        let bytes = envelope::sign(
            &Unsigned {
                name: parsed.name.clone(),
                seq: parsed.seq,
                time: parsed.time,
                handoff: parsed.handoff.clone(),
                lockers,
            },
            &admin.seed,
        )
        .unwrap();
        transport::publish(&admin.config.source, &bytes).unwrap();
        client::sync(paths, None).unwrap();
    }
}

#[cfg(test)]
mod tests;
