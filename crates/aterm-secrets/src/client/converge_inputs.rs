// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What this Mac's converge pass (`aterm init --converge`) reads from its vault that is no opened
//! entry, digested, so an apply can say whether the version it applied owes a pass
//! ([`SyncOutcome::Applied`](super::SyncOutcome::Applied)'s `moved`).
//!
//! Every opened entry, whatever its class (held by name, a control record such as the fleet
//! code or the swarm roster, policy and lease, a Claude Code token, one set apart), is in the
//! applied selection's meaning, and [`Report::changed`](super::Report) says when one moved. The
//! pass reads more than the entries, and none of it shows there:
//!
//! - the membership log's verdicts on who is out (removed, denied, left out of `[everyone]`), on
//!   the node keys no member holds now, and on this Mac's own seat, label and audiences;
//! - the log's own swarm roster where it denies this Mac or a label or node, which the denial
//!   reads take beside the envelope's while the envelope carries no stamp, and the numbers other
//!   members chose, which decide whether this Mac may serve under its host's label;
//! - the Commons control where the log seals it under its own epochs, which is no opened entry;
//! - on an admin's Mac, whose steward steps read both whole: the admin text (less the `[join]`
//!   notes every publish rewrites with its own version) and the membership, and the log's
//!   verdict on each `[machines]` row, seat or none.
//!
//! What it leaves out is what moves all day and no item reads: every other Mac's ordinary record
//! (a network observation, a `Put` of an entry, which `Report::changed` answers where it changed
//! the value), the log's heads, and another member's seat, so a Mac let in moves nothing on a
//! Mac that is no admin.
//!
//! An apply that finds a difference leaves the pass owed in `state.toml` ([`owe`]), and only a
//! process that starts runners takes it ([`take_owed`]): an apply made by `aterm secrets sync`,
//! a publish's own sync or a reader records the new digest and meaning, and the next apply
//! would read no difference.
//!
//! Not here, and no vault version carries it: the Commons table of credential requests
//! (`credentials/requests/1` in aterm-db), which `sync.requests` reads. A request another Mac
//! posts there is seen by this Mac's pass only when something else starts one
//! (`docs/KNOWN_ISSUES.md`).

use super::{Identity, Paths, State};
use crate::envelope::Envelope;
use crate::membership_log::{EpochAudience, Log, NodeStanding};
use crate::text::Document;
use crate::{SecretBytes, age};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// The admin text of the applied envelope as this Mac holds it.
pub(super) enum Admin {
    /// No admin locker is addressed to this Mac (or the vault is held as a log alone).
    None,
    /// The admin text this Mac's identity opens.
    Text(SecretBytes),
    /// An admin locker this Mac could not judge, by its sealed bytes: they differ in every
    /// version, so each owes a pass rather than none.
    Unread(Vec<u8>),
}

impl Admin {
    /// `envelope`'s admin text as `identity` opens it.
    pub(super) fn of(envelope: &Envelope, identity: &Identity) -> Self {
        let Some(locker) = envelope.admin_locker() else {
            return Self::None;
        };
        match age::decrypt(std::slice::from_ref(identity), locker.armored.as_bytes()) {
            Ok(plain) => Self::Text(plain),
            Err(age::AgeError::NoMatch) => Self::None,
            Err(_) => Self::Unread(locker.armored.as_bytes().to_vec()),
        }
    }
}

/// What the log and the admin text say that the pass reads, digested.
pub(super) struct Inputs(aterm_digest::Sha256);

impl Inputs {
    /// Read what `log` and `admin` say that the pass reads, as `identity` sees it.
    pub(super) fn read(log: Option<&Log>, identity: &Identity, admin: Admin) -> Self {
        let mut hash = aterm_digest::Sha256::new();
        hash.update(b"aterm/converge-inputs/2\0");
        let whole = !matches!(admin, Admin::None);
        let mut machines = Vec::new();
        match admin {
            Admin::None => field(&mut hash, "admin", b""),
            Admin::Text(text) => match steward_text(&text) {
                Some((custody, document)) => {
                    field(&mut hash, "admin-custody", custody);
                    field(&mut hash, "admin-document", document.render().as_slice());
                    machines = document.machines();
                }
                // A text this build cannot read as a document is hashed as it is: each version
                // that differs owes a pass.
                None => field(&mut hash, "admin-text", text.as_slice()),
            },
            Admin::Unread(sealed) => field(&mut hash, "admin-unread", &sealed),
        }
        if let Some(log) = log {
            let rows: Vec<age::Recipient> = machines
                .iter()
                .filter_map(|machine| age::Recipient::parse(&machine.recipient).ok())
                .collect();
            membership(&mut hash, log, identity, &rows);
            if whole {
                field(
                    &mut hash,
                    "membership",
                    format!("{:?}", log.membership_ref()).as_bytes(),
                );
            }
        }
        Self(hash)
    }

    /// Record the digest in `state`, and say whether it differs from the one the last apply
    /// recorded. A state that recorded none differs.
    pub(super) fn record(self, state: &mut State) -> bool {
        let digest: [u8; 32] = self.0.finalize().into();
        let moved = state.converge_inputs != Some(digest);
        state.converge_inputs = Some(digest);
        #[cfg(test)]
        if BLIND.with(std::cell::Cell::get) {
            return false;
        }
        moved
    }
}

/// The admin text as the steward's steps read it: its custody lines as they are, and its
/// document without `[join]`'s `code-only` notes. A publish rewrites those notes with its own
/// version (`note_code_only`), so the text as written differs in every version of a vault that
/// holds one `code-only` line; they tell an approver which keys stayed named, and no item of the
/// pass reads them. `None` for a text with no document this build can read.
fn steward_text(text: &SecretBytes) -> Option<(&[u8], Document)> {
    let (custody, document) = text.as_str()?.split_once("\n\n")?;
    let mut document = Document::parse(document).ok()?;
    document.set_code_only_notes(&BTreeMap::new(), None).ok()?;
    Some((custody.as_bytes(), document))
}

/// Leave a converge pass owed in `state`, where `moved`: the apply that found the difference
/// may be one whose caller starts no runner.
pub(super) fn owe(state: &mut State, moved: bool) {
    state.converge_owed |= moved;
}

/// How long [`take_owed`] waits for the vault's state lock. A writer still there keeps the debt
/// for the next round.
const TAKE_WAIT: Duration = Duration::from_millis(250);

/// Take the converge pass an apply left owed ([`owe`]), for a caller that then asks for the
/// runner: the vault's subscriber and the node pass, after each round, whatever that round's own
/// outcome. A caller that ends before its runner starts loses nothing: each starts one at its
/// first look, and so takes what is owed as it begins and asks for no second runner. `false` where none is owed, or the state cannot be read or written now (it stays owed).
pub(super) fn take_owed(paths: &Paths) -> bool {
    if !super::load_state(paths).is_ok_and(|state| state.converge_owed) {
        return false;
    }
    let Ok(_lock) = super::bounded::take(paths, Some(Instant::now() + TAKE_WAIT)) else {
        return false;
    };
    let Ok(mut state) = super::load_state(paths) else {
        return false;
    };
    if !state.converge_owed {
        return false;
    }
    state.converge_owed = false;
    super::save_state(paths, &state).is_ok()
}

#[cfg(test)]
thread_local! {
    /// The negative control of the digest, on for one test thread: an apply records it and
    /// never says it moved, as an apply that read only its report did.
    pub(super) static BLIND: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `apply` with the digest blind ([`BLIND`]).
#[cfg(test)]
pub(super) fn blind<T>(apply: impl FnOnce() -> T) -> T {
    /// Sight again when the apply ends, by a panic too.
    struct Sighted;
    impl Drop for Sighted {
        fn drop(&mut self) {
            BLIND.with(|blind| blind.set(false));
        }
    }
    BLIND.with(|blind| blind.set(true));
    let _sighted = Sighted;
    apply()
}

/// One labelled, length-prefixed part.
fn field(hash: &mut aterm_digest::Sha256, label: &str, bytes: &[u8]) {
    hash.update((label.len() as u64).to_be_bytes());
    hash.update(label.as_bytes());
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

/// A part by its `Debug` rendering: of public keys, record ids and labels only, in the sorted
/// order their maps and sets keep. Another build's rendering is another digest, which costs that
/// build's first apply one pass.
fn shown(hash: &mut aterm_digest::Sha256, label: &str, part: &dyn std::fmt::Debug) {
    field(hash, label, format!("{part:?}").as_bytes());
}

/// What the pass's items read from `log` on any Mac: who is out, and where this Mac stands.
/// `machines` are the admin document's `[machines]` recipients, on a Mac that reads it.
fn membership(
    hash: &mut aterm_digest::Sha256,
    log: &Log,
    identity: &Identity,
    machines: &[age::Recipient],
) {
    let membership = log.membership_ref();
    let own = identity.recipient();
    let everyone = EpochAudience::Everyone;
    shown(hash, "removed", membership.removed());
    shown(hash, "removed-recipients", membership.removed_recipients());
    shown(hash, "removed-labels", membership.removed_labels());
    shown(hash, "revoking-rows", membership.revoking_rows());
    shown(hash, "quorums", &membership.quorums());
    shown(hash, "voided", membership.voided());
    // A member's own current key is its seat's news; every other standing takes a key out.
    let keys: Vec<_> = membership
        .nodes()
        .iter()
        .filter(|(_, standing)| !matches!(standing, NodeStanding::Member(_)))
        .collect();
    shown(hash, "node-keys", &keys);
    // Every seat and waiting founder the log's own verdicts take out, as the steward's cascade
    // and each Mac's denial reads judge them.
    let mut out = BTreeSet::new();
    for (actor, member) in membership.members() {
        let recipient = member.binding.recipient();
        if log.recipient_is_removed(&recipient)
            || log.audience_left_out(&everyone, actor)
            || log.audience_left_out_recipient(&everyone, &recipient)
            || membership.member_node_removed(member)
            || super::log_entries::recipient_denied_by_roster_or_log(None, log, &recipient)
        {
            out.insert(recipient);
        }
    }
    for founder in membership.unproved() {
        if log.recipient_is_removed(&founder.recipient)
            || log.audience_left_out_recipient(&everyone, &founder.recipient)
            || super::log_entries::recipient_denied_by_roster_or_log(None, log, &founder.recipient)
        {
            out.insert(founder.recipient);
        }
    }
    // The steward's cascade judges each `[machines]` row, and a row may have no seat: a key a
    // re-key left out with no `Remove` is out by its missing wrap alone.
    for recipient in machines {
        if log.recipient_is_removed(recipient)
            || log.audience_left_out_recipient(&everyone, recipient)
            || super::log_entries::recipient_denied_by_roster_or_log(None, log, recipient)
        {
            out.insert(*recipient);
        }
    }
    shown(hash, "out", &out);
    // The log's own swarm roster: whether it denies this Mac, and the labels and nodes it
    // denies. Under an envelope with no stamp the opened entries are the envelope's, and the
    // denial reads take the log's roster beside them (`log_entries::denial_roster`).
    match super::log_entries::log_denial_roster(log, identity) {
        Ok(roster) => {
            let denied =
                super::log_entries::recipient_denied_by_roster_or_log(roster.as_ref(), log, &own);
            let denies = roster.map(|roster| (roster.deny_labels, roster.deny_nodes));
            shown(hash, "own-denied", &(denied, denies));
        }
        Err(_) => field(hash, "own-denied-unread", b""),
    }
    // The numbers other members chose: the log renames their roster rows with no change of the
    // roster entry, and a row that takes this host's label ends this Mac's own use of it
    // (`steward::resolve_member_own_label`, `roster::resolve_local_swarm_label`). A choice is
    // rare; an admission makes none.
    let chosen: Vec<_> = membership
        .members()
        .iter()
        .filter(|(_, member)| member.binding.recipient() != own)
        .filter_map(|(actor, member)| {
            Some((
                member.binding.recipient(),
                membership.label_after_choice(actor)?,
            ))
        })
        .collect();
    shown(hash, "chosen-labels", &chosen);
    // This Mac's own seats: its label (a number choice, or the suffix a lost one leaves), name,
    // node key and entry, and whether each audience still reaches it.
    for (actor, member) in membership
        .members()
        .iter()
        .filter(|(_, member)| member.binding.recipient() == own)
    {
        shown(hash, "own-seat", &(actor, member));
        shown(hash, "own-chosen", &membership.chosen_label(actor));
        shown(hash, "own-choice", &membership.label_after_choice(actor));
        shown(
            hash,
            "own-commons-seat",
            &log.audience_left_out(&EpochAudience::Commons, actor),
        );
    }
    for founder in membership
        .unproved()
        .iter()
        .filter(|founder| founder.recipient == own)
    {
        shown(hash, "own-founder", founder);
    }
    shown(
        hash,
        "own-reach",
        &(
            log.wraps_to(&own),
            log.audience_left_out_recipient(&EpochAudience::Commons, &own),
            log.has_commons_epochs(),
        ),
    );
    // The Commons control under its own epochs is sealed apart from the opened entries.
    if log.has_commons_epochs() {
        match log.open_commons_control(identity) {
            Ok(Some(control)) => field(hash, "commons-control", control.as_slice()),
            Ok(None) => field(hash, "commons-control-none", b""),
            Err(_) => field(hash, "commons-control-unread", b""),
        }
    }
}

#[cfg(test)]
#[path = "converge_inputs_tests.rs"]
mod tests;
