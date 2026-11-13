// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    let bytes = client::seal_with_admissions_and_parents(
        &admin.config.name,
        admin.seq() + 1,
        &admin.seed,
        &admin.commons_seed,
        admin.commons_bound,
        &admin.doc,
        &admin.handoff,
        &[],
        &crate::client::admission::Ledger::default()
            .retained(&admin.doc)
            .unwrap(),
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
            parents: parsed.parents.clone(),
            lockers,
        },
        &admin.seed,
    )
    .unwrap();
    transport::publish(&admin.config.source, &bytes).unwrap();
    client::sync(paths, None).unwrap();
}
