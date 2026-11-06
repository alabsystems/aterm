// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A member authenticates its public log and this fresh exchange before possession.

use super::{desk, joiner::Greeting};
use crate::{Error, Result};

const DOMAIN: &[u8] = b"aterm/alab/proof-authority/1\0";

pub(super) fn message(
    nonce: &[u8; 32],
    greeting: &Greeting,
    proof_line: &str,
    records: &[u8],
) -> Vec<u8> {
    let mut message = DOMAIN.to_vec();
    message.extend_from_slice(nonce);
    // Reuse the complete greeting serialization with its separate, fixed legacy nonce slot;
    // the authority nonce above is all 32 fresh bytes, never truncated to that slot's size.
    message.extend_from_slice(&desk::greet_message(
        &[0; desk::GREET_NONCE_LEN],
        greeting,
        desk::Answer::Allowed,
    ));
    message.extend_from_slice(&(proof_line.len() as u64).to_be_bytes());
    message.extend_from_slice(proof_line.as_bytes());
    message.extend_from_slice(&aterm_digest::Sha256::digest(records));
    message
}

pub(super) fn verify(
    records: &[u8],
    greeting: &Greeting,
    nonce: &[u8; 32],
    proof_line: &str,
    signature: &[u8; 64],
    bound: crate::crossing::RecordsBound,
) -> Result<()> {
    use crate::membership_log::{Body, EpochAudience, Store, decode_records_bounded};
    if !astream_aead::signature::verify(
        &greeting.key,
        &message(nonce, greeting, proof_line, records),
        signature,
    ) {
        return Err(Error::new(
            "the member did not authenticate this authority exchange",
        ));
    }
    let mut store = Store::for_vault(crate::ALAB_VAULT)?;
    let decoded = decode_records_bounded(records, bound.held as usize)?;
    let offered: std::collections::BTreeSet<_> =
        decoded.iter().map(|record| *record.id()).collect();
    if offered.len() != decoded.len() {
        return Err(Error::new(
            "the member's authority repeats membership records",
        ));
    }
    store.ingest_records(decoded)?;
    if store.inventory() != offered || store.complete_inventory() != offered {
        return Err(Error::new(
            "the member's authority is not a complete admitted history",
        ));
    }
    let log = store
        .log()
        .ok_or_else(|| Error::new("the member's authority has no compiled ALab genesis"))?;
    let Body::Genesis { root, .. } = log.genesis().body() else {
        return Err(Error::new("the member's authority has no genesis"));
    };
    if root.network != greeting.network || root.authority != greeting.authority {
        return Err(Error::new("the member's greeting names another authority"));
    }
    let membership = log.membership();
    if !membership.members().iter().any(|(actor, member)| {
        member.binding.writer() == &greeting.key
            && !log.recipient_is_removed(&member.binding.recipient())
            && !membership.member_node_removed(member)
            && !log.audience_left_out(&EpochAudience::Everyone, actor)
            && !log
                .audience_left_out_recipient(&EpochAudience::Everyone, &member.binding.recipient())
    }) {
        return Err(Error::new(
            "the responder has no current member binding in ALab",
        ));
    }
    Ok(())
}
