// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pure functions for editing [`Roster`] values before master signing.

use aterm_update_core::roster::{MAX_MACHINES, Machine, Roster, SUPPORTED_SCHEMA};

/// The `valid_until` every mint and revocation stamps: keys do not expire.
pub const VALID_UNTIL_FOREVER: &str = "9999-12-31T00:00:00Z";

/// Initial empty roster at sequence 0.
#[must_use]
pub fn empty(_now_unix: u64) -> Roster {
    Roster {
        schema: SUPPORTED_SCHEMA,
        roster_seq: 0,
        valid_until: VALID_UNTIL_FOREVER.to_string(),
        machines: Vec::new(),
        revoked: Vec::new(),
    }
}

/// Add a machine, advancing the sequence and restamping `valid_until`.
/// Refuses duplicate or revoked machine IDs and duplicate public keys.
pub fn add(mut r: Roster, id: &str, pubkey_b64: &str, now_unix: u64) -> Result<Roster, String> {
    if id.is_empty() {
        return Err(
            "a machine id is required (it is how a release is attributed, and \
                    how a revocation names its target)"
                .to_string(),
        );
    }
    if r.machines.iter().any(|m| m.id == id) {
        let mut s = String::from("machine id '");
        s.push_str(id);
        s.push_str(
            "' is already on the roster. Reusing an id REPLACES its key and leaves two live \
             authorities under one name — mint under a new id (e.g. '",
        );
        s.push_str(id);
        s.push_str("-2'), then revoke the old one.");
        return Err(s);
    }
    if r.is_revoked(id) {
        let mut s = String::from("machine id '");
        s.push_str(id);
        s.push_str("' has been revoked; an id never returns. Mint under a new one.");
        return Err(s);
    }
    if let Some(twin) = r.machines.iter().find(|m| m.pubkey == pubkey_b64) {
        let mut s = String::from("that public key is already on the roster as '");
        s.push_str(&twin.id);
        s.push_str(
            "'. One key under two ids cannot be revoked: authority is decided by key and \
             revocation names an id, so revoking either leaves the other signing with the \
             same key. Mint this machine its own key, or revoke '",
        );
        s.push_str(&twin.id);
        s.push_str("' if this IS that machine.");
        return Err(s);
    }
    if r.machines.len() >= MAX_MACHINES {
        return Err(
            "the roster is full. Every machine on it can sign any release for \
                    every user, so the ceiling is deliberate — revoke one before adding \
                    another."
                .to_string(),
        );
    }
    r.machines.push(Machine {
        id: id.to_string(),
        pubkey: pubkey_b64.to_string(),
        added_at: aterm_types::rfc3339::format_rfc3339(now_unix),
        not_after: None,
    });
    bump(r)
}

/// Remove a machine from `[[machine]]`, record it in `revoked`, and advance `roster_seq`.
///
/// Revoking an `id` already present in `revoked` succeeds with `machines` and `revoked`
/// unchanged while still advancing `roster_seq` and restamping `valid_until`.
///
/// # Errors
///
/// Returns `Err` if `id` appears in neither `machines` nor `revoked`, or if `roster_seq` is
/// already `u64::MAX`.
pub fn revoke(mut r: Roster, id: &str, _now_unix: u64) -> Result<Roster, String> {
    if !r.machines.iter().any(|m| m.id == id) && !r.is_revoked(id) {
        let mut s = String::from("machine id '");
        s.push_str(id);
        s.push_str("' is not on the roster — nothing to revoke. Check the id.");
        return Err(s);
    }
    r.machines.retain(|m| m.id != id);
    if !r.is_revoked(id) {
        r.revoked.push(id.to_string());
    }
    bump(r)
}

fn bump(mut r: Roster) -> Result<Roster, String> {
    r.roster_seq = r
        .roster_seq
        .checked_add(1)
        .ok_or_else(|| "roster sequence has reached u64::MAX".to_string())?;
    r.valid_until = VALID_UNTIL_FOREVER.to_string();
    Ok(r)
}

#[cfg(test)]
#[path = "roster_ops_tests.rs"]
mod tests;
