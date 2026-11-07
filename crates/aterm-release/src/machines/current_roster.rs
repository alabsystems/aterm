// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Current master-signed roster selection shared by live signing and provisioning.
//! Reads authenticated local evidence only; optional network results come from callers.

use crate::{
    ledger::{Error, Result},
    machines,
};
use aterm_update_core::roster::{self, Roster};
use std::path::Path;

/// A roster pair verified under the supplied release-master anchors and parsed.
#[cfg(unix)]
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub(crate) bytes: Vec<u8>,
    pub(crate) sig: Vec<u8>,
    pub(crate) roster: Roster,
}

/// Verify-then-parse.
#[cfg(unix)]
pub(crate) fn admit_candidate(
    master_pubkeys: &[&str],
    bytes: Vec<u8>,
    sig: Vec<u8>,
) -> std::result::Result<Candidate, String> {
    let verified = roster::verify_roster(master_pubkeys, bytes.clone(), &sig).map_err(|e| {
        format!("the roster did not verify under the committed paper master ({e:?})")
    })?;
    let parsed = Roster::parse(&verified)
        .map_err(|e| format!("the roster verified but did not parse ({e:?})"))?;
    Ok(Candidate {
        bytes,
        sig,
        roster: parsed,
    })
}

/// Read a local pair under the supplied anchors, without changing either half.
#[cfg(unix)]
pub(crate) fn read_local_candidate_with(
    roster_path: &Path,
    master_pubkeys: &[&str],
) -> Result<Option<Candidate>> {
    read_local_candidate_using(roster_path, master_pubkeys, Path::try_exists)
}

fn read_local_candidate_using(
    roster_path: &Path,
    master_pubkeys: &[&str],
    exists: impl Fn(&Path) -> std::io::Result<bool>,
) -> Result<Option<Candidate>> {
    atpkg_keys::provision::refuse_pending_roster_transaction(&utf8_path(roster_path)?)
        .map_err(Error::new)?;
    let sig_path = machines::RosterDocument::signature_path(roster_path);
    let body = exists(roster_path)
        .map_err(|error| Error::new(format!("inspect {}: {error}", roster_path.display())))?;
    let sig = exists(&sig_path)
        .map_err(|error| Error::new(format!("inspect {}: {error}", sig_path.display())))?;
    if !body && !sig {
        atpkg_keys::provision::refuse_pending_roster_transaction(&utf8_path(roster_path)?)
            .map_err(Error::new)?;
        return Ok(None);
    }
    if body != sig {
        let (present, absent) = if body {
            (roster_path.display(), sig_path.display())
        } else {
            (sig_path.display(), roster_path.display())
        };
        return Err(Error::new(format!(
            "half a roster pair: {present} exists but {absent} is missing. The pair is \
             one authorization document — re-copy BOTH files from the machine that has \
             them, or remove the stray file to accept the channel release's pair",
        )));
    }
    let doc = machines::RosterDocument::read(roster_path)?;
    let c = admit_candidate(master_pubkeys, doc.bytes, doc.signature).map_err(|e| {
        Error::new(format!(
            "the roster pair at {} is unusable: {e}. Refusing to overwrite local \
                 roster state this tool cannot prove — if it was a hand-copy, re-copy \
                 BOTH files from the source machine; remove both files to accept the \
                 channel release's pair instead",
            roster_path.display(),
        ))
    })?;
    atpkg_keys::provision::refuse_pending_roster_transaction(&utf8_path(roster_path)?)
        .map_err(Error::new)?;
    Ok(Some(c))
}

/// The refusal of a selected roster below the retained release floor.
#[cfg(unix)]
pub(crate) const BELOW_RETAINED_FLOOR: &str =
    "selected public roster is below the retained release floor";

#[cfg(unix)]
pub(crate) fn require_roster_floor(candidate: &Candidate, floor: u64) -> Result<()> {
    if candidate.roster.roster_seq < floor {
        Err(Error::new(BELOW_RETAINED_FLOOR))
    } else {
        Ok(())
    }
}

/// The currently authenticated public pair and the existing retained authority floor.
#[cfg(unix)]
pub(crate) fn public_vault_candidate(
    paths: &aterm_secrets::client::Paths,
    masters: &[&str],
    read: impl FnOnce(
        &aterm_secrets::client::Paths,
    ) -> aterm_secrets::Result<aterm_secrets::client::PublicRosterRead>,
) -> Result<(Option<Candidate>, u64)> {
    use aterm_secrets::client::PublicRosterRead;
    let floor = aterm_secrets::client::release::roster_floor(paths)
        .map_err(|error| Error::new(error.to_string()))?;
    let candidate = match read(paths).map_err(|error| Error::new(error.to_string()))? {
        PublicRosterRead::Unconfigured | PublicRosterRead::Absent => None,
        PublicRosterRead::Pair(pair) => {
            Some(admit_candidate(masters, pair.roster, pair.signature).map_err(Error::new)?)
        }
    };
    Ok((candidate, floor))
}

/// Recheck the observed public source immediately before effects or final success.
#[cfg(unix)]
pub(crate) fn require_public_source(
    paths: &aterm_secrets::client::Paths,
    observed: Option<&Candidate>,
    floor: u64,
    masters: &[&str],
    read: impl FnOnce(
        &aterm_secrets::client::Paths,
    ) -> aterm_secrets::Result<aterm_secrets::client::PublicRosterRead>,
) -> Result<()> {
    let (current, current_floor) = public_vault_candidate(paths, masters, read)?;
    if current_floor != floor || !same_candidate(observed, current.as_ref()) {
        Err(Error::new(
            "authenticated public roster changed during provisioning",
        ))
    } else {
        Ok(())
    }
}

#[cfg(unix)]
pub(crate) fn same_candidate(left: Option<&Candidate>, right: Option<&Candidate>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => left.bytes == right.bytes,
        _ => false,
    }
}

/// Refuse a changed effective pair rather than reporting an earlier authorization.
///
/// A pair whose lock exists is read under it. A pair no roster-protocol writer has locked
/// (the vault places the kept pair as two plain files, outside the protocol) is read
/// without it, and nothing is created. Every protocol writer
/// ([`atpkg_keys::provision::lock_roster`]) creates the lock before it writes either half or
/// a transaction, so a lock still absent after the read shows that none of them touched the
/// pair while it was read, and the read stands. One that appeared sends the read back under
/// that lock. A write outside the protocol is caught here as it is under the lock: by the
/// master signature, the half-pair check and the comparison with `selected`.
#[cfg(unix)]
pub(crate) fn require_public_pair(
    path: &Path,
    selected: &Candidate,
    masters: &[&str],
) -> Result<()> {
    require_public_pair_around(path, selected, masters, || {})
}

/// [`require_public_pair`], with `between` run after an unlocked pair is read and before its
/// lock is looked at again.
#[cfg(unix)]
fn require_public_pair_around(
    path: &Path,
    selected: &Candidate,
    masters: &[&str],
    between: impl FnOnce(),
) -> Result<()> {
    let body = utf8_path(path)?;
    let lock = atpkg_keys::provision::roster_lock_path(&body);
    if never_locked(&lock)? {
        let unlocked = require_public_pair_held(path, selected, masters);
        between();
        if never_locked(&lock)? {
            return unlocked;
        }
    }
    let _guard = atpkg_keys::provision::lock_roster_read_only(&body).map_err(Error::new)?;
    require_public_pair_held(path, selected, masters)
}

/// Whether the roster lock at `lock` is absent: no roster-protocol writer has taken it.
#[cfg(unix)]
fn never_locked(lock: &str) -> Result<bool> {
    match std::fs::symlink_metadata(lock) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Ok(_) => Ok(false),
        Err(error) => Err(Error::new(format!("inspect roster lock {lock}: {error}"))),
    }
}

/// Read under the caller's roster writer or read-only lock, or by [`require_public_pair`] for
/// a pair no protocol writer has locked, which keeps the result only while the lock stays
/// absent.
#[cfg(unix)]
pub(crate) fn require_public_pair_held(
    path: &Path,
    selected: &Candidate,
    masters: &[&str],
) -> Result<()> {
    atpkg_keys::provision::refuse_pending_roster_transaction(&utf8_path(path)?)
        .map_err(Error::new)?;
    let current = read_local_candidate_with(path, masters)?;
    if same_candidate(current.as_ref(), Some(selected)) {
        Ok(())
    } else {
        Err(Error::new(
            "effective public roster changed during provisioning",
        ))
    }
}

fn read_local_candidates(paths: &[&Path], masters: &[&str]) -> Result<Vec<Candidate>> {
    let mut candidates = Vec::new();
    for path in paths {
        if let Some(candidate) = read_local_candidate_with(path, masters)? {
            candidates.push(candidate);
        }
    }
    Ok(candidates)
}

/// All observed generations must be coherent, including lower generations whose
/// successors would otherwise hide the conflicting evidence.
pub(crate) fn select_all(candidates: Vec<Candidate>) -> Result<Option<Candidate>> {
    for (index, candidate) in candidates.iter().enumerate() {
        if candidates[..index].iter().any(|prior| {
            prior.roster.roster_seq == candidate.roster.roster_seq && prior.bytes != candidate.bytes
        }) {
            return Err(Error::new(
                "LINEAGE FORK: different master-signed public rosters share one generation",
            ));
        }
    }
    Ok(candidates
        .into_iter()
        .max_by_key(|candidate| candidate.roster.roster_seq))
}

#[cfg(unix)]
pub(crate) fn utf8_path(p: &Path) -> Result<String> {
    p.to_str()
        .map(str::to_string)
        .ok_or_else(|| Error::new(format!("{} is not valid UTF-8", p.display())))
}

/// One observed current authority selection; not a lease across later effects.
pub(crate) struct CurrentRoster {
    pub(crate) selected: Candidate,
    pub(crate) observed: Option<Candidate>,
    pub(crate) floor: u64,
}

/// Resolve every admitted input before applying signer-specific authorization.
/// A configured unreadable public source refuses, rather than falling back to old grants.
pub(crate) fn resolve(
    home: &Path,
    paths: &[&Path],
    masters: &[&str],
    extra: std::result::Result<Option<Candidate>, String>,
    read: impl FnOnce(
        &aterm_secrets::client::Paths,
    ) -> aterm_secrets::Result<aterm_secrets::client::PublicRosterRead>,
) -> Result<CurrentRoster> {
    let source = aterm_secrets::client::Paths::for_home(home);
    let (observed, floor) = public_vault_candidate(&source, masters, read)?;
    let mut candidates = read_local_candidates(paths, masters)?;
    if let Some(public) = observed.as_ref() {
        candidates.push(public.clone());
    }
    let unavailable = match extra {
        Ok(Some(extra)) => {
            candidates.push(extra);
            None
        }
        Ok(None) => None,
        Err(error) => Some(error),
    };
    let selected = select_all(candidates)?.ok_or_else(|| {
        Error::new(unavailable.map_or_else(
            || "no admitted current public roster".into(),
            |error| format!("no admitted current public roster: {error}"),
        ))
    })?;
    require_roster_floor(&selected, floor)?;
    Ok(CurrentRoster {
        selected,
        observed,
        floor,
    })
}

impl CurrentRoster {
    pub(crate) fn document(&self) -> machines::RosterDocument {
        machines::RosterDocument {
            bytes: self.selected.bytes.clone(),
            signature: self.selected.sig.clone(),
        }
    }

    pub(crate) fn recheck(&self, home: &Path, masters: &[&str]) -> Result<()> {
        require_public_source(
            &aterm_secrets::client::Paths::for_home(home),
            self.observed.as_ref(),
            self.floor,
            masters,
            aterm_secrets::client::current_public_roster,
        )
    }

    /// Require the public payload actually consumed by the source-signing engine.
    pub(crate) fn require_canonical(&self, home: &Path, masters: &[&str]) -> Result<()> {
        let canonical = home.join(atpkg_keys::provision::RELEASE_ROSTER_REL);
        require_public_pair(&canonical, &self.selected, masters).map_err(|error| {
            Error::new(format!(
                "current public roster is not bound to the canonical signing ring: {error}; \
                 restore the ring's pair from the channel's: download its aterm-machines.toml \
                 and .sig into an empty folder, then `targo --unverified ship provision --id \
                 <this Mac's signer id> --roster-only --roster <that aterm-machines.toml>` \
                 (`--check` first)"
            ))
        })
    }
}

#[cfg(test)]
#[path = "current_roster_tests.rs"]
mod tests;
