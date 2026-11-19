// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This Mac's pass over its format-2 membership log (`docs/DESIGN-vault-log-2026-11-14.md`
//! step 4, tracker FL-148F), run at the end of each vault pass (`client::sync`): it shares the
//! checkpoint its log proposes, prunes at the newest final checkpoint, and sheds the bodies
//! their own signers replaced ([`crate::membership_log::pass`] decides each).
//!
//! **The gate** ([`run`]): it answers before it reads anything on a Mac whose build reads no
//! format 2 ([`crate::crossing::FormatMark::follows`]: none does while
//! [`crate::commons::hearing::READS_FORMAT`] names no release, which holds
//! [`crate::commons::hearing::FENCE`] shut), and reads only the format mark on a Mac whose mark
//! names no format 2: a mark is stored only by the first format-2 write, once its writer's own
//! fence held fourteen days (`client::format_two`), or on a seat's signed version-2 record
//! (`crate::membership_log::follow`). A Mac with no mark writes nothing here, signs nothing and
//! keeps no record. A test opens the gate only through its node's faults
//! ([`crate::crossing::Faults::follows`] and [`crate::crossing::Faults::now_ms`]).
//!
//! **In order**, each step on the store as the last left it:
//!
//! 1. *Share*, as an author writes ([`super::log_entries`]): a seated Mac signs the share its
//!    log drafts ([`crate::membership_log::Log::share_draft`]), asked as a membership write by
//!    the class stops, and holds it in its records like any record it signs; the next pass's
//!    exchange posts it.
//! 2. *Prune*, under the state lock: the store pruned at each final checkpoint whose shares are
//!    more than half of the seats its log holds ([`crate::membership_log::Store::adopt_final`])
//!    is written as a rooted store is (`client::write_records`, whose `persist_stored`
//!    rewrites the entry through `crate::records_storage::rewrite` from the bytes it read).
//! 3. *Shed*, under the same lock: the store with its replaced bodies shed
//!    ([`crate::membership_log::Store::shed_superseded`]) is written by a rewrite of the entry
//!    whose intent names the shed set (`client::write_shed`). No vote.
//!
//! A crash at any step leaves the entry as the rewrite's pin says, whole, and the next pass
//! makes the same prune or shed again (a pure function of the records).
//!
//! **What a person sees** ([`LOG_PASS_FILE`]): the last pass that did something keeps a
//! journal record beside the records, read on demand by `aterm secrets status` ([`line`]) and
//! by the survey's `vault_log` row ([`row`]). Never an alert.

use super::Paths;
use crate::membership_log::RecordId;

/// The journal record of this Mac's last log pass that did something ([`Done`]), beside its
/// records: `aterm-secrets log pass 1`, then `at`, `shed`, `shared` and `pruned` lines.
pub const LOG_PASS_FILE: &str = "log-pass";

const MAGIC: &str = "aterm-secrets log pass 1";

/// What one pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Done {
    /// When it ran, by this Mac's clock (Unix milliseconds).
    pub at_ms: u64,
    /// How many records' bodies it shed.
    pub shed: usize,
    /// The checkpoint it signed a share of, by its id.
    pub shared: Option<RecordId>,
    /// The final checkpoint it pruned at, by its id.
    pub pruned: Option<RecordId>,
}

impl Done {
    /// Whether it changed anything.
    #[must_use]
    pub const fn did(&self) -> bool {
        self.shed > 0 || self.shared.is_some() || self.pruned.is_some()
    }

    fn encode(&self) -> String {
        let id = |id: Option<RecordId>| id.map_or_else(|| "-".to_owned(), |id| hex(&id));
        format!(
            "{MAGIC}\nat {}\nshed {}\nshared {}\npruned {}\n",
            self.at_ms,
            self.shed,
            id(self.shared),
            id(self.pruned)
        )
    }

    fn decode(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != MAGIC {
            return None;
        }
        let mut field = |word: &str| lines.next()?.strip_prefix(word)?.strip_prefix(' ');
        let id = |text: &str| -> Option<Option<RecordId>> {
            if text == "-" {
                return Some(None);
            }
            let mut id = [0; 32];
            aterm_codec::hex::decode_into(text.as_bytes(), &mut id).then_some(Some(id))
        };
        let done = Self {
            at_ms: field("at")?.parse().ok()?,
            shed: field("shed")?.parse().ok()?,
            shared: id(field("shared")?)?,
            pruned: id(field("pruned")?)?,
        };
        lines.next().is_none().then_some(done)
    }
}

fn hex(id: &RecordId) -> String {
    aterm_codec::hex::encode_lower(id)
}

/// The record of this Mac's last log pass that did something; `None` where none is kept or it
/// does not read, and, reading nothing past the format mark, on a Mac whose build reads no
/// format 2 or whose mark names none (the module's gate, as [`run`]'s): a file such a Mac holds
/// (left by a build it was downgraded from, or copied in with the vault folder) states a pass
/// this build never ran, and its status and survey say nothing of it.
#[must_use]
pub fn last(paths: &Paths) -> Option<Done> {
    if !gate_open(paths) {
        return None;
    }
    let bytes = crate::fsx::read_private(&paths.dir.join(LOG_PASS_FILE), 1024).ok()??;
    Done::decode(std::str::from_utf8(bytes.as_slice()).ok()?)
}

/// Whether this Mac runs its log pass: its build reads format 2 and its mark names it.
#[cfg(unix)]
fn gate_open(paths: &Paths) -> bool {
    #[cfg(test)]
    if crate::crossing::faults(super::format_mark(paths).path()).log_pass_ungated {
        return true;
    }
    let mark = super::format_mark(paths);
    mark.follows() && mark.stored().ok() == Some(crate::crossing::FORMAT_TWO)
}

/// No Mac of this platform stores a format mark.
#[cfg(not(unix))]
fn gate_open(_: &Paths) -> bool {
    false
}

/// `aterm secrets status`' line for this Mac's last log pass ([`last`]), `when` saying when it
/// ran from its Unix seconds; `None` where none is kept, which is every Mac whose build reads
/// no format 2.
#[must_use]
pub fn line(paths: &Paths, when: impl FnOnce(u64) -> String) -> Option<String> {
    let done = last(paths)?;
    let short = |id: &RecordId| hex(id)[..12].to_owned();
    let mut did = Vec::new();
    if let Some(id) = &done.shared {
        did.push(format!("shared checkpoint {}", short(id)));
    }
    if let Some(id) = &done.pruned {
        did.push(format!("pruned at checkpoint {}", short(id)));
    }
    if done.shed > 0 {
        did.push(format!(
            "shed {} replaced bod{}",
            done.shed,
            if done.shed == 1 { "y" } else { "ies" }
        ));
    }
    Some(format!(
        "log pass: {} ({})",
        did.join(", "),
        when(done.at_ms / 1000)
    ))
}

/// The survey's `vault_log` row for this Mac's last log pass ([`last`]): `vault_log at=<ms>
/// shed=<n> shared=<id|-> pruned=<id|->`; `None` where none is kept.
#[must_use]
pub fn row(paths: &Paths) -> Option<String> {
    let done = last(paths)?;
    let id = |id: Option<RecordId>| id.map_or_else(|| "-".to_owned(), |id| hex(&id));
    Some(format!(
        "vault_log at={} shed={} shared={} pruned={}",
        done.at_ms,
        done.shed,
        id(done.shared),
        id(done.pruned)
    ))
}

/// Run this Mac's log pass (the module's steps) and keep its record where it did something.
/// `None`, with nothing read past the format mark and nothing written, where this Mac's build
/// reads no format 2 or its mark names no format 2 (the module's gate).
///
/// # Errors
/// The records or this Mac's signer cannot be read, the state lock cannot be taken,
/// a share cannot be signed or committed, a prune or a shed fails or cannot be written, or the
/// record cannot be written.
#[cfg(unix)]
pub(crate) fn run(paths: &Paths) -> crate::Result<Option<Done>> {
    if !gate_open(paths) {
        return Ok(None);
    }
    let now = super::format_two::now_ms(paths);
    // A share that cannot be signed (this Mac's signer unreadable) keeps no prune or shed
    // from running; its error is the pass's once they ran.
    let shared = share(paths, now);
    let (pruned, shed) = {
        let _lock = super::StateLock::take(paths)?;
        let mut store = super::load_records(paths)?;
        if !store.reading().admits_two() {
            return Ok(None);
        }
        let pruned = match store.adopt_final()? {
            Some((next, root)) => {
                super::write_records(paths, &next)?;
                store = next;
                Some(root)
            }
            None => None,
        };
        let shed = match store.shed_superseded()? {
            Some((next, ids)) => {
                super::write_shed(paths, &next, &ids)?;
                ids.len()
            }
            None => 0,
        };
        (pruned, shed)
    };
    let shared = shared?;
    let done = Done {
        at_ms: now,
        shed,
        shared,
        pruned,
    };
    if done.did() {
        crate::fsx::write_atomic(
            &paths.dir.join(LOG_PASS_FILE),
            done.encode().as_bytes(),
            0o600,
        )?;
    }
    Ok(Some(done))
}

/// Step 1: sign and hold this Mac's share of the checkpoint its log drafts at `now`, where it
/// is seated and the share is asked through as a membership write; its id.
#[cfg(unix)]
fn share(paths: &Paths, now: u64) -> crate::Result<Option<RecordId>> {
    use crate::membership_log::budgets::{Author, Authoring, Class};
    let (_, _, shared) = super::log_entries::author_and_commit_records(paths, |store| {
        let Some(log) = store.log() else {
            return Ok((Vec::new(), None));
        };
        let Some((actor, device, incarnation)) =
            super::log_entries::open_local_member_signer(paths, log)?
        else {
            return Ok((Vec::new(), None));
        };
        let Some(draft) = log.share_draft(&actor, now)? else {
            return Ok((Vec::new(), None));
        };
        let share = device.with_seed(|seed| draft.sign(seed, incarnation))?;
        let batch = Authoring::all(std::slice::from_ref(&share), Class::Membership);
        if log.author_ask(&batch, Author::Person, &actor, now).is_err() {
            return Ok((Vec::new(), None));
        }
        let id = crate::membership_log::checkpoint::checkpoint_id(share.body());
        Ok((vec![share], id))
    })?;
    Ok(shared)
}

#[cfg(all(test, unix))]
#[path = "log_pass_tests.rs"]
mod tests;
