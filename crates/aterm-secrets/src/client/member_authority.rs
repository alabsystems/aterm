// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Public membership evidence for a member's pre-possession authority proof.

use super::{Error, Paths, StateLock, log_entries};
use crate::commons::bootstrap::Device;
use crate::members::desk::Proving;
use crate::membership_log::{Body, EpochAudience, MAX_RECORDS_STREAM, encode_records};

/// The complete acknowledged public membership records this member may serve before
/// a new Mac proves possession. Secret entries and epoch keys remain sealed in those
/// records; neither plaintext entries nor opened keys enter this payload. The desk
/// binds the exact bytes to its greeting, the request and a fresh joiner nonce.
///
/// This is a local observation: it starts no network request, creates no identity,
/// and refuses a busy vault rather than waiting behind a writer.
///
/// # Errors
/// Sharing is off or unreadable, the vault is busy or not current, this device is
/// no admitted member, its request names another root, its reader cannot take the
/// source's history, or the public stream exceeds its bounded wire format.
pub fn member_authority(paths: &Paths, proving: &Proving<'_>) -> crate::Result<Vec<u8>> {
    let sharing = || -> crate::Result<()> {
        if crate::swarm::enabled_in_home(&paths.home)? {
            Ok(())
        } else {
            Err(Error::new(crate::swarm::SWARM_OFF))
        }
    };
    sharing()?;
    let lock = StateLock::observe(paths)?
        .ok_or_else(|| Error::new("member authority is busy or unavailable"))?;
    let config = super::config(paths)?
        .ok_or_else(|| Error::new("member authority has no configured vault"))?;
    let log = log_entries::trusted_local_log(paths, &config)?
        .ok_or_else(|| Error::new("member authority has no current acknowledged log"))?;
    let Body::Genesis { root, .. } = log.genesis().body() else {
        return Err(Error::new("member authority has no membership genesis"));
    };
    if !root.names(proving.request.binding()) {
        return Err(Error::new("member authority request names another root"));
    }
    let store = super::load_records(paths)?;
    if proving.lab && super::live_genesis_store(&store) {
        return Err(Error::new(crate::members::desk::LAB_REFUSED));
    }
    let identity = super::read_identity(paths)?
        .ok_or_else(|| Error::new("member authority has no local identity"))?;
    let binding = Device::existing_binding_for_observation(paths, &identity)?
        .ok_or_else(|| Error::new("member authority has no local device"))?;
    let membership = log.membership();
    let member = membership
        .get(&binding.actor())
        .filter(|member| {
            member.binding.machine() == binding.machine()
                && member.binding.recipient() == binding.recipient()
                && member.binding.writer() == binding.writer()
                && member.binding.incarnation() == binding.incarnation()
        })
        .ok_or_else(|| Error::new("member authority does not seat this device"))?;
    let roster = log_entries::local_swarm_roster_for_log_held(paths, &log, &lock)?;
    if log.recipient_is_removed(&member.binding.recipient())
        || membership.member_node_removed(member)
        || log.audience_left_out(&EpochAudience::Everyone, &binding.actor())
        || log.audience_left_out_recipient(&EpochAudience::Everyone, &member.binding.recipient())
        || roster
            .as_ref()
            .is_some_and(|roster| log_entries::member_denied_by_roster(roster, member))
    {
        return Err(Error::new("member authority is no longer admitted"));
    }
    // Retention may hold more than the acknowledged projection. Its durable
    // crossed state still governs every exchange, including a public witness.
    let bound = super::records_bound(paths, &store)?;
    if !proving.reader.can_take(bound) {
        return Err(Error::new(proving.reader.refusal(bound)));
    }
    let bytes = log.records().try_fold(0usize, |total, record| {
        total
            .checked_add(record.bytes().len())
            .filter(|total| *total <= MAX_RECORDS_STREAM)
            .ok_or_else(|| Error::new("member authority exceeds the public record stream bound"))
    })?;
    let witness = encode_records(log.records());
    debug_assert_eq!(witness.len(), bytes);
    sharing()?;
    Ok(witness)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::log_entries_tests::two_macs;
    use crate::client::{Stored, load_records, load_state, save_state};
    use crate::crossing::RecordsBound;
    use crate::members::bundle::Reader;
    use crate::text::Audience;
    use std::cell::Cell;

    #[test]
    fn authority_is_public_current_bounded_and_read_only() {
        let macs = two_macs(&[(
            Audience::Everyone,
            "private-metadata",
            b"private-vault-value",
        )]);
        let log = load_records(&macs.m22).unwrap().log().unwrap().clone();
        let Body::Genesis { root, .. } = log.genesis().body() else {
            panic!("fixture genesis");
        };
        let device = Device::open(&macs.m7).unwrap().unwrap();
        let request = device.request(root.network, root.authority).unwrap();
        let fresh = Cell::new(false);
        let mut proving = Proving {
            request: &request,
            node: [7; 32],
            name: "joining",
            reader: Reader::BoundedLog(RecordsBound::uncrossed()),
            fleet: false,
            lan: true,
            lab: false,
            fresh: &fresh,
        };
        let before = std::fs::read(macs.m22.state_file()).unwrap();
        let records = std::fs::read(macs.m22.records_file()).unwrap();
        let witness = member_authority(&macs.m22, &proving).unwrap();
        assert!(!witness.is_empty());
        assert!(witness.len() <= MAX_RECORDS_STREAM);
        assert!(
            !witness
                .windows(b"private-vault-value".len())
                .any(|part| part == b"private-vault-value")
        );
        assert!(
            !witness
                .windows(b"private-metadata".len())
                .any(|part| part == b"private-metadata")
        );
        let mut verified = crate::membership_log::Store::with_compiled(Some(*log.network()));
        verified.ingest(&witness).unwrap();
        assert_eq!(verified.log().unwrap().heads(), log.heads());
        assert_eq!(std::fs::read(macs.m22.state_file()).unwrap(), before);
        assert_eq!(std::fs::read(macs.m22.records_file()).unwrap(), records);
        assert!(!fresh.get(), "public evidence signs no admission");

        proving.lab = true;
        let refused =
            crate::client::with_live_genesis(true, || member_authority(&macs.m22, &proving))
                .unwrap_err();
        assert!(
            refused
                .to_string()
                .contains(crate::members::desk::LAB_REFUSED)
        );
        proving.lab = false;
        let foreign = device.request([8; 16], [9; 32]).unwrap();
        proving.request = &foreign;
        assert!(member_authority(&macs.m22, &proving).is_err());
        proving.request = &request;
        proving.reader = Reader::Files;
        assert!(member_authority(&macs.m22, &proving).is_err());
        proving.reader = Reader::BoundedLog(RecordsBound::uncrossed());
        let held = StateLock::take(&macs.m22).unwrap();
        assert!(member_authority(&macs.m22, &proving).is_err());
        drop(held);
        let mut state = load_state(&macs.m22).unwrap();
        state.failed = true;
        save_state(&macs.m22, &state).unwrap();
        assert!(member_authority(&macs.m22, &proving).is_err());
        state.failed = false;
        state.membership = Stored::Pending;
        save_state(&macs.m22, &state).unwrap();
        assert!(member_authority(&macs.m22, &proving).is_err());
        macs.server.stop();
    }
}
