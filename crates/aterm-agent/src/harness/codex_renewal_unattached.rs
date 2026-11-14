// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A login renewal's successor that never attached, kept against that successor. The root's end
//! hands its session back on the login it was on, where the same wall asks for the same successor
//! again: without this record each new process (a new index key) would mint a new root onto it at
//! once, and each would signal the agent and end the same way.

use super::*;

/// One successor's ends for want of attaching: the last root that ended so, how many have, and
/// when the last did (Unix seconds).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Unattached {
    root: String,
    ends: u32,
    at: i64,
}

impl Unattached {
    /// Until when (Unix seconds) the record holds its successor off: the rung of a wall that names
    /// no reset ([`accounts::NO_RESET_WALL_S`]) for its count of ends, from the last.
    fn until(&self) -> i64 {
        self.at
            .saturating_add(accounts::rung(&accounts::NO_RESET_WALL_S, self.ends))
    }
}

/// The record of the successor a login renewal of `identity` selects: the login `new_auth` of
/// sign-in generation `generation`. A new sign-in is a new generation, and so a new record.
fn unattached_path(
    env: &Env,
    identity: &Identity,
    new_auth: &Path,
    generation: u64,
) -> Result<PathBuf, &'static str> {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm/codex-renewal/unattached/v1\0");
    for part in [
        identity.account.as_bytes(),
        identity.user.as_bytes(),
        new_auth.as_os_str().as_encoded_bytes(),
    ] {
        hash.update(part);
        hash.update(b"\0");
    }
    hash.update(generation.to_be_bytes());
    let digest = aterm_codec::hex::encode_lower(&hash.finalize());
    Ok(env
        .accounts_store_for(accounts::Cli::Codex)
        .ok_or("unattached-store")?
        .dir
        .join("renewal-follow")
        .join(format!("unattached-{digest}.json")))
}

impl ReceiptWriter<'_> {
    /// Hold off the successor login renewal `root` selected, now that it ended because that
    /// successor never attached ([`CancellationKind::SuccessorNeverAttached`]): a root onto it is
    /// not created again ([`Self::create`]) until [`Self::unattached_login_until`] passes, by
    /// the rungs of a wall that names no reset ([`accounts::NO_RESET_WALL_S`]), counted as a
    /// wall's sightings are ([`accounts::note_wall`]): an end while the hold still stands is the
    /// same failure (its root was started before the hold, which no new root passes) and changes
    /// nothing; an end more than [`accounts::WALL_EPISODE_S`] after the hold lapsed starts over at
    /// the first rung; any other climbs one. The same root counts once, so a retried cleanup is
    /// exact, and an attach clears the record ([`Self::note_attached_login`]).
    /// # Errors
    /// The root is not a retired login renewal its cancellation ended with no successor, it
    /// changed, or the record cannot be read or durably written.
    pub fn note_unattached_login(&self, root: &Receipt) -> Result<(), &'static str> {
        if root.intent.purpose != Purpose::Login
            || root.phase != Phase::Retired
            || !root.revoked
            || root.successor_pid.is_some()
            || root.successor_birth.is_some()
            || root.accepted_turn.is_some()
            || read_cancellation(self.env, root)?.map(|cancellation| cancellation.reason)
                != Some(CancellationKind::SuccessorNeverAttached)
            || read_receipt(self.env, &root.id).as_ref() != Some(root)
        {
            return Err("unattached-login-not-terminal");
        }
        let path = unattached_path(
            self.env,
            &root.intent.identity,
            &root.new_auth,
            root.generation,
        )?;
        let prior: Option<Unattached> = read_record(&path)?;
        if prior.as_ref().is_some_and(|prior| prior.root == root.id) {
            // A cleanup that lost its acknowledgement repairs it; the end is not counted twice.
            return flush_parent(&path);
        }
        let ends = match &prior {
            None => 1,
            Some(prior) if self.env.now < prior.until() => return flush_parent(&path),
            Some(prior)
                if self.env.now.saturating_sub(prior.until()) > accounts::WALL_EPISODE_S =>
            {
                1
            }
            Some(prior) => prior.ends.saturating_add(1),
        };
        let next = Unattached {
            root: root.id.clone(),
            ends,
            at: self.env.now,
        };
        let text = encoded(&next)?;
        private_parent(&path)?;
        super::super::super::atomic_write::write_atomic_mode(&path, &text, Some(0o600))
            .map_err(|_| "unattached-write")?;
        flush_parent(&path)
    }

    /// Clear the hold-off of the successor login renewal `root` selected, now that it attached:
    /// `root`'s successor registered ([`Phase::Committed`], or [`Phase::Consumed`] after it), so
    /// that login works, and the ends without a successor that held it off
    /// ([`Self::note_unattached_login`]) start over from none.
    /// # Errors
    /// The root is not a login renewal whose successor registered, it changed, or the record
    /// cannot be removed durably.
    pub fn note_attached_login(&self, root: &Receipt) -> Result<(), &'static str> {
        if root.intent.purpose != Purpose::Login
            || !matches!(root.phase, Phase::Committed | Phase::Consumed)
            || root.successor_pid.is_none()
            || root.successor_birth.is_none()
            || read_receipt(self.env, &root.id).as_ref() != Some(root)
        {
            return Err("attached-login-not-committed");
        }
        let path = unattached_path(
            self.env,
            &root.intent.identity,
            &root.new_auth,
            root.generation,
        )?;
        match std::fs::remove_file(&path) {
            Ok(()) => flush_parent(&path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err("unattached-remove"),
        }
    }

    /// Until when (Unix seconds) a login renewal of `identity` onto the login `new_auth` of
    /// sign-in generation `generation` waits, because a root onto it ended with no successor
    /// ([`Self::note_unattached_login`]); `None` once that has passed, or where none did.
    /// # Errors
    /// The record cannot be read safely.
    pub fn unattached_login_until(
        &self,
        identity: &Identity,
        new_auth: &Path,
        generation: u64,
    ) -> Result<Option<i64>, &'static str> {
        let path = unattached_path(self.env, identity, new_auth, generation)?;
        let Some(record) = read_record::<Unattached>(&path)? else {
            return Ok(None);
        };
        let until = record.until();
        Ok((self.env.now < until).then_some(until))
    }
}

#[cfg(test)]
#[path = "codex_renewal_unattached_tests.rs"]
mod tests;
