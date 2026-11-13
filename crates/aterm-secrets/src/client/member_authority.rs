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
#[path = "member_authority_tests.rs"]
mod tests;
