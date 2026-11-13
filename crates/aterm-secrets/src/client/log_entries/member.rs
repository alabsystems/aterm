// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Member writes (`docs/DESIGN-claude-setup-token-2026-11-05.md` §3.4–§3.6; part of one-mesh
//! B13): a seated member's `aterm secrets set` signs its own `[everyone]` `Put` into the vault's
//! membership log, and its `aterm secrets unset` its own `Delete`, with no admin key and no
//! envelope. The log already counts any member's `Put` or `Delete` (a seat in its causal past is
//! the only authority it needs), so what holds a member back is the classification
//! ([`crate::entry_class`]): only a `Shared` name is a member's to write.
//!
//! An unset never waits: it drafts its `Delete` under a fresh `[everyone]` epoch, wrapped to no
//! removed Mac, and seals every entry that stays again under it (F15), so the epoch that sealed
//! the removed value is neither newest nor winning and no Mac admitted afterwards is handed its
//! key ([`member_unset`]).
//!
//! The write never judges a stale store: it drafts under [`author_and_commit_records_until`],
//! whose draft re-runs where the records moved before the commit, and it signs nothing where a
//! writer here waits (one-mesh M4): a removal's rotation not here yet, a newest epoch this Mac
//! does not open, or an earlier write of this Mac's of the name. Such a write waits in
//! `everyone.waiting` for the next vault pass ([`sign_waiting_writes`]), sealed to this Mac's own
//! recipient, and reads "not yet in vault" until it is signed and a server took it. The draft
//! claim ([`claim_draft`]) keeps a pass's signing of waiting writes out from before the draft
//! until the write waits; it does not hold back applies, syncs or a rotation arriving, which the
//! draft's own checks judge.

use super::waiting::{
    WaitingWrite, everyone_fresh_to, everyone_seal, held_defers, land_member_write,
    overtaken_since, overtaker, sign_everyone, still_waits, take_back_member_write, waits_here,
};
use super::*;

/// How this Mac writes its vault (§3.4), as [`write_route`] reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteRoute {
    /// A vault at a path, or one whose Mac holds no trusted log: an admin writes it, by the
    /// admin locker.
    NoLog,
    /// A trusted log that seats no device key of this Mac: it writes nothing, and never falls back
    /// to the admin locker.
    NotSeated,
    /// A vault pass held this Mac's state past the deadline: nothing is known of the seat.
    Busy,
    /// A seat in the log: a member write.
    Seated,
}

/// What a member write does when this Mac's vault is busy past its deadline.
#[derive(Clone, Copy, Debug)]
pub enum OnBusy {
    /// Refuse ([`MemberWritten::Busy`]): the value is still the person's, so nothing is lost.
    Refuse(std::time::Instant),
    /// Wait in `everyone.waiting` for the next vault pass ([`WaitFor::LockDeadline`]).
    Wait(std::time::Instant),
}

impl OnBusy {
    const fn deadline(self) -> std::time::Instant {
        match self {
            Self::Refuse(by) | Self::Wait(by) => by,
        }
    }
}

/// Why a member write waits for the next vault pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitFor {
    /// A removal's rotation has not reached this Mac: sealed under a newest epoch that still
    /// wraps the removed Mac, the value would reach it (one-mesh M4).
    RotationDue,
    /// This Mac does not hold the key of every newest `[everyone]` epoch.
    NewestUnopened,
    /// Concurrent newest epochs the tender has not coalesced, or a batch still arriving that may
    /// wrap a Mac outside the seats: there is no epoch to seal under yet.
    KeysUnsettled,
    /// An earlier write of this Mac's of the name waits or is pending, which this one replaces.
    PendingOfThisMac,
    /// A vault pass held this Mac's state past the deadline ([`OnBusy::Wait`]).
    LockDeadline,
}

/// What a member write did, decided by reading the log and the waiting file again afterwards,
/// never from what a pass returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemberWritten {
    /// Signed, and a server took it.
    Posted,
    /// Signed into this Mac's log, and no server took it yet: not yet in vault. This Mac's next
    /// records pass posts what a server lacks; its vault pass posts only what that pass signs.
    SignedHere,
    /// Not signed yet: it waits in `everyone.waiting` for this Mac's vault pass. Not yet in vault.
    Waiting(WaitFor),
    /// Another write took the name first: the op that holds it now, where the log names one.
    Overtaken(Option<Overtaker>),
    /// Refused, signing nothing and writing no waiting file: why.
    Refused(String),
    /// A vault pass held this Mac's state past the deadline ([`OnBusy::Refuse`]).
    Busy,
    /// This Mac has no seat in the vault's log ([`WriteRoute::NotSeated`]).
    NotSeated,
    /// This Mac's vault has no trusted log ([`WriteRoute::NoLog`]): an admin writes it.
    NoLog,
}

/// What a member's `set` changes of an if-absent entry's anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// The anchor the entry has stays (`set` with neither `--with` nor `--no-with`).
    Keep,
    /// The entry is anchored to this one (`--with <name>`).
    To(String),
    /// The entry loses its anchor (`--no-with`).
    Drop,
}

/// One member `set`: an `[everyone]` entry and how it fills.
#[derive(Clone)]
pub struct MemberEntry {
    /// The entry's name.
    pub name: String,
    /// Its value, at most [`crate::text::MAX_VALUE`] bytes.
    pub value: SecretBytes,
    /// Written only where the machine has no file of its own (`--if-absent`).
    pub if_absent: bool,
    /// What becomes of its anchor.
    pub anchor: Anchor,
    /// `--fleet-reads-anchors`: every Mac reads anchors, so the vault's first anchored entry may
    /// go in.
    pub fleet_reads_anchors: bool,
}

/// Why the vault's first anchored entry waits for the fleet: no machine tells the vault which
/// aterm it runs, so the person who sets it says it (`--fleet-reads-anchors`), on an admin's `set`
/// and a member's ([`member_set`]) alike.
pub const FIRST_ANCHOR: &str = "this is the vault's first anchored entry (if-absent=<anchor>), and an aterm older than anchors refuses every version that holds one: a machine on it applies nothing until it updates, and an admin on it cannot open the vault. Update every machine and admin first (there `aterm secrets --help` names --with)";

/// How this Mac writes its vault (§3.4): a vault at a URL whose trusted log seats this Mac's
/// device key is [`WriteRoute::Seated`]; a vault at a path, or one with no trusted log, is
/// [`WriteRoute::NoLog`]. The state is observed until `by` at most: a pass that holds it is
/// [`WriteRoute::Busy`], never read as no log.
///
/// # Errors
/// The configuration, the records or this Mac's device key cannot be read.
pub fn write_route(paths: &Paths, by: std::time::Instant) -> crate::Result<WriteRoute> {
    // A vault at a path has no server a member's records reach other Macs through.
    if super::super::config(paths)?.is_none_or(|config| matches!(config.source, Source::Path(_))) {
        return Ok(WriteRoute::NoLog);
    }
    let Some(store) = super::super::remove::trusted_store(paths)? else {
        return Ok(WriteRoute::NoLog);
    };
    let Some(log) = store.log() else {
        return Ok(WriteRoute::NoLog);
    };
    StateLock::narrow_owned(paths)?;
    let Some(lock) = StateLock::observe_until(paths, by)? else {
        if paths.lock_file().exists() {
            return Ok(WriteRoute::Busy);
        }
        return Err(Error::new("this Mac's vault state has no lock file"));
    };
    Ok(
        match open_local_member_signer_held_until(paths, log, &lock, None)? {
            Some(_) => WriteRoute::Seated,
            None => WriteRoute::NotSeated,
        },
    )
}

/// The log's `[everyone]` value of `name` as this Mac opens it now, and whether concurrent values
/// stand beside it: what a write that waits because the draft never ran is signed over.
fn viewed(paths: &Paths, identity: &Identity, name: &str) -> (Option<Entry>, bool) {
    let opened = super::super::remove::trusted_store(paths)
        .ok()
        .flatten()
        .and_then(|store| store.log()?.open_audience_entries(identity).ok()?);
    let Some(opened) = opened else {
        return (None, false);
    };
    let key = (Audience::Everyone, name.to_owned());
    (
        opened
            .by_audience
            .get(&Audience::Everyone)
            .and_then(|entries| entries.get(name))
            .cloned(),
        opened.conflicts.contains(&key),
    )
}

/// What the draft of a member write made, one item per entry, in order.
enum Drafted {
    /// The `Put`s it signed, or the ops that hold the same values already.
    Signed(Vec<RecordId>),
    /// Nothing signed: the writes wait.
    Waits(WaitFor, Vec<WaitingWrite>),
    /// Nothing signed: refused.
    Refused(String),
    /// Nothing signed: the draft found no seat.
    NotSeated,
}

/// Why a Mac that opens the Hosts audience `hosts`, which holds `name` too, refuses a member
/// write of it (§3.3): a `set`, or where `unset`, an unset.
fn hosts_refusal(name: &str, hosts: &Audience, unset: bool) -> String {
    let (write, admin) = if unset {
        ("unset", "`unset --for` removes")
    } else {
        ("write", "`set --for` changes")
    };
    format!(
        "`{name}` is also held for {}, which this Mac is one of: a member {write} would leave \
         those copies; until B13, an admin's {admin} them",
        hosts.header()
    )
}

/// Why a member set of `tos` names an anchor no Mac would hold (§3.5, as an admin's `set` refuses
/// it in `text::check_anchors`): every Mac's view drops an entry whose anchor is no if-absent
/// entry of what it receives, so the `Put` would be in every server's log and on no Mac, read as
/// overtaken. The anchor is judged in `[everyone]` as the write leaves it, `everyone` with `tos`
/// in place; `None` where every anchor holds.
fn dangling_anchor(everyone: Option<&BTreeMap<String, Entry>>, tos: &[Entry]) -> Option<String> {
    let find = |anchor: &str| {
        tos.iter()
            .find(|to| to.name == anchor)
            .or_else(|| everyone?.get(anchor))
            .map(|entry| entry.if_absent)
    };
    let to = tos
        .iter()
        .find(|to| !crate::text::anchor_received_fields(&to.name, to.anchor.as_deref(), find))?;
    Some(format!(
        "`{}` is anchored to `{}`, which is not an if-absent entry in [everyone]: set `{}` \
         --if-absent first",
        to.name,
        to.anchor.as_deref().unwrap_or_default(),
        to.anchor.as_deref().unwrap_or_default(),
    ))
}

/// The member draft of `entries` (§3.5 step 3), all in one draft, against `store`: refused where
/// a Hosts audience this Mac opens holds a name, where a name would make a folded pair, where an
/// anchor names no if-absent entry ([`dangling_anchor`]), or where a fill does not hold (as an
/// admin's `set` refuses it); waiting where a writer here waits; else
/// the `Put`s, sealed as a vault pass seals ([`everyone_seal`]) under one epoch at one
/// `authored_ms`, each after a `Delete` where the log holds concurrent values for its name, which
/// the person's write settles. Where the log holds every entry as it would leave it already,
/// nothing is signed; where it holds some, every entry is signed again, so the draft stays one.
fn draft_member_set(
    paths: &Paths,
    store: &Store,
    identity: &Identity,
    entries: &[MemberEntry],
    now_ms: u64,
) -> crate::Result<(Vec<Record>, Drafted)> {
    let nothing = |drafted| Ok((Vec::new(), drafted));
    let Some(log) = store.log() else {
        return nothing(Drafted::NotSeated);
    };
    let Some((actor, device, incarnation)) = open_local_member_signer(paths, log)? else {
        return nothing(Drafted::NotSeated);
    };
    let Some(opened) = log.open_audience_entries(identity)? else {
        let mut writes = Vec::new();
        for entry in entries {
            match planned(entry, None) {
                Ok(to) => writes.push(WaitingWrite::member(to, None, false)),
                Err(why) => return nothing(Drafted::Refused(why.to_string())),
            }
        }
        return nothing(Drafted::Waits(WaitFor::NewestUnopened, writes));
    };
    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    for name in &names {
        for (audience, held) in &opened.by_audience {
            if matches!(audience, Audience::Hosts(_)) && held.contains_key(*name) {
                return nothing(Drafted::Refused(hosts_refusal(name, audience, false)));
            }
        }
        let held = opened
            .by_audience
            .values()
            .flat_map(BTreeMap::keys)
            .map(String::as_str)
            .filter(|held| !names.contains(held));
        if let Some((_, other)) = crate::entry_class::folded_pairs(held.chain([*name]))
            .into_iter()
            .find(|(this, _)| this == name)
        {
            return nothing(Drafted::Refused(crate::entry_class::folded_pair_refusal(
                name, other,
            )));
        }
    }
    let everyone = opened.by_audience.get(&Audience::Everyone);
    let anchored = opened
        .by_audience
        .values()
        .flat_map(BTreeMap::values)
        .any(|held| held.anchor.is_some());
    let mut tos = Vec::with_capacity(entries.len());
    let mut reads = Vec::with_capacity(entries.len());
    let mut conflicted = Vec::with_capacity(entries.len());
    for entry in entries {
        let read = everyone.and_then(|held| held.get(&entry.name));
        let to = match planned(entry, read) {
            Ok(to) => to,
            Err(why) => return nothing(Drafted::Refused(why.to_string())),
        };
        if to.anchor.is_some() && !anchored && !entry.fleet_reads_anchors {
            return nothing(Drafted::Refused(format!(
                "{FIRST_ANCHOR}, then pass --fleet-reads-anchors"
            )));
        }
        conflicted.push(
            opened
                .conflicts
                .contains(&(Audience::Everyone, entry.name.clone())),
        );
        tos.push(to);
        reads.push(read);
    }
    if let Some(why) = dangling_anchor(everyone, &tos) {
        return nothing(Drafted::Refused(why));
    }
    let waits = |why| {
        let writes = tos
            .iter()
            .zip(&reads)
            .zip(&conflicted)
            .map(|((to, read), conflicted)| WaitingWrite::member(to.clone(), *read, *conflicted))
            .collect();
        nothing(Drafted::Waits(why, writes))
    };
    // An earlier write of this Mac's of a name, still to be signed, would land over this one:
    // this one replaces it where it waits, even where the log already holds this value.
    if names.iter().any(|name| waits_here(paths, identity, name)) {
        return waits(WaitFor::PendingOfThisMac);
    }
    // A name a held draft of this Mac changes is this Mac's own edit in flight, which the log
    // read here does not hold yet: signed now, this write would meet it as a concurrent write
    // once that publish keeps it. It waits for the vault pass, which defers it until the keep or
    // the hold's going (one-mesh P3); a hold that does not read may change any name.
    if held_defers(paths, store, identity, &names) {
        return waits(WaitFor::PendingOfThisMac);
    }
    let holding: Option<Vec<RecordId>> = tos
        .iter()
        .zip(&reads)
        .zip(&conflicted)
        .map(|((to, read), conflicted)| {
            (!conflicted && read.is_some_and(|read| same_entry(read, to)))
                .then(|| {
                    opened
                        .winning_puts
                        .get(&(Audience::Everyone, to.name.clone()))
                        .copied()
                })
                .flatten()
        })
        .collect();
    if let Some(holding) = holding {
        return nothing(Drafted::Signed(holding));
    }
    if rotation_due(log, &opened, &EpochAudience::Everyone) {
        return waits(WaitFor::RotationDue);
    }
    if !opens_newest(&opened, &EpochAudience::Everyone) {
        return waits(WaitFor::NewestUnopened);
    }
    let Some(seal) = everyone_seal(log, &opened, identity) else {
        return waits(WaitFor::KeysUnsettled);
    };
    let deletes: Vec<&String> = entries
        .iter()
        .zip(&conflicted)
        .filter(|(_, conflicted)| **conflicted)
        .map(|(entry, _)| &entry.name)
        .collect();
    let puts: Vec<&Entry> = tos.iter().collect();
    let mut log = log.clone();
    let minted = sign_everyone(
        &mut log,
        &opened,
        (&actor, &device, incarnation),
        now_ms,
        seal,
        &deletes,
        &puts,
    )?;
    let signed: Vec<RecordId> = minted
        .iter()
        .rev()
        .take(puts.len())
        .rev()
        .map(|record| *record.id())
        .collect();
    if signed.len() != puts.len() {
        return Err(Error::new("the member write signed no Put"));
    }
    Ok((minted, Drafted::Signed(signed)))
}

/// The `[everyone]` entry `entry` makes over `was`, the value the log holds at its name, as an
/// admin's `set` makes it (`cli::set_in`): an if-absent entry stays one only by `--if-absent`,
/// and keeps its anchor unless `--no-with` drops it.
///
/// # Errors
/// `was` fills gaps and `entry` does not say `--if-absent`, or the entry is invalid (an anchor
/// without if-absent, or one that names the entry itself).
fn planned(entry: &MemberEntry, was: Option<&Entry>) -> crate::Result<Entry> {
    let name = &entry.name;
    if !entry.if_absent
        && let Some(was) = was.filter(|was| was.if_absent)
    {
        let (spelled, kept) = match &was.anchor {
            Some(anchor) => (format!("if-absent={anchor}"), " (its anchor stays)"),
            None => ("if-absent".to_owned(), ""),
        };
        return Err(Error::new(format!(
            "{name} fills gaps ({spelled}): pass --if-absent to keep it so{kept}, or unset it first to make it a plain entry"
        )));
    }
    let anchor = match &entry.anchor {
        Anchor::To(anchor) => Some(anchor.clone()),
        Anchor::Drop => None,
        Anchor::Keep => was.and_then(|was| was.anchor.clone()),
    };
    if let Some(anchor) = &anchor
        && (!entry.if_absent || !crate::text::valid_entry_name(anchor) || anchor == name)
    {
        return Err(Error::new(
            "an anchor goes with if-absent and names another entry (letters, digits, . _ -; or ~/a/b)",
        ));
    }
    Ok(Entry {
        name: name.clone(),
        value: entry.value.clone(),
        audience: Audience::Everyone,
        if_absent: entry.if_absent,
        anchor,
    })
}

/// Whether a member write of this Mac's of `name` is still to be signed: it waits in
/// `everyone.waiting`, or a pending set holds it. Where either does not read, it may hold the name,
/// and so it does: "not yet in vault" until the next read says otherwise.
///
/// # Errors
/// This Mac's identity cannot be read.
pub(crate) fn member_waits_here(paths: &Paths, name: &str) -> crate::Result<bool> {
    let identity = super::super::read_identity(paths)?
        .ok_or_else(|| Error::new("this Mac has no secrets identity"))?;
    Ok(waits_here(paths, &identity, name))
}

/// A fixture's hold on this Mac's waiting writes, as a vault pass takes it while it signs them:
/// while it lives, a member write's draft waits for it (`OnBusy`). Released when dropped.
#[cfg(any(test, debug_assertions))]
pub struct TestPassClaim {
    _held: super::waiting::DraftClaim,
}

/// Take the vault pass's claim on `paths`' waiting writes in an isolated fixture: `None` while a
/// member write holds its own claim.
///
/// # Errors
/// The claim's lock cannot be opened.
#[cfg(any(test, debug_assertions))]
pub fn test_hold_pass_claim(paths: &Paths) -> crate::Result<Option<TestPassClaim>> {
    Ok(super::waiting::pass_claim(paths)?.map(|held| TestPassClaim { _held: held }))
}

#[cfg(test)]
thread_local! {
    /// A test's action between a member write's posts and its outcome's read: another Mac's
    /// write, or a vault pass, landing there.
    pub(in super::super) static BEFORE_OUTCOME: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Write `entry` into `[everyone]` as this Mac, a seated member (§3.5), with no admin key and no
/// envelope; only a `Shared` name ([`crate::entry_class::member_refusal`]). Under `on_busy`'s
/// deadline it takes the draft claim, drafts under [`author_and_commit_records_until`] (signing,
/// or waiting, or refusing, [`draft_member_set`]), lands a write that waits in `everyone.waiting`,
/// lets the claim go, posts what it signed to every server this Mac reaches, and decides the
/// outcome by reading the log and the waiting file again. A refusal signs nothing and writes no
/// waiting file. It applies nothing to this Mac's files: its caller syncs where it wants them.
///
/// # Errors
/// This Mac's state or records cannot be read or written, a record fails to sign or keep, or the
/// waiting write cannot be landed.
pub fn member_set(
    paths: &Paths,
    entry: &MemberEntry,
    on_busy: OnBusy,
) -> crate::Result<MemberWritten> {
    if let Some(why) = crate::entry_class::member_refusal(&entry.name) {
        return Ok(MemberWritten::Refused(why));
    }
    member_write(paths, std::slice::from_ref(entry), on_busy)
}

/// [`member_set`] of `entries`, whose names the caller judged ([`crate::entry_class`]), in one
/// draft: one actor, one `authored_ms` and one epoch (`claude_token::write`'s pair). An empty or
/// oversized value is refused. Where the writes wait, each waits in `everyone.waiting`, and the
/// vault pass signs them together. The outcome is the least settled of the entries': overtaken,
/// then waiting, then signed here, then posted.
///
/// # Errors
/// As [`member_set`].
pub(crate) fn member_write(
    paths: &Paths,
    entries: &[MemberEntry],
    on_busy: OnBusy,
) -> crate::Result<MemberWritten> {
    for entry in entries {
        let name = &entry.name;
        if entry.value.is_empty() {
            return Ok(MemberWritten::Refused(format!(
                "the value is empty; aterm secrets unset {name} removes an entry"
            )));
        }
        if entry.value.len() > crate::text::MAX_VALUE {
            return Ok(MemberWritten::Refused(
                "the value is larger than 1 MiB".to_owned(),
            ));
        }
    }
    let by = on_busy.deadline();
    let busy = || match on_busy {
        OnBusy::Refuse(_) => None,
        OnBusy::Wait(_) => Some(WaitFor::LockDeadline),
    };
    let lock_deadline = match write_route(paths, by)? {
        WriteRoute::NoLog => return Ok(MemberWritten::NoLog),
        WriteRoute::NotSeated => return Ok(MemberWritten::NotSeated),
        WriteRoute::Busy => busy(),
        WriteRoute::Seated => None,
    };
    let identity = super::super::read_identity(paths)?
        .ok_or_else(|| Error::new("this Mac has no secrets identity"))?;
    let started_ms = aterm_time::unix_millis().unwrap_or(0);
    let claim = match claim_draft(paths, Some(by)) {
        Ok(claim) => Some(claim),
        Err(_) if std::time::Instant::now() >= by => None,
        Err(error) => return Err(error),
    };
    let drafted = match (&claim, lock_deadline) {
        (Some(_), None) => author_and_commit_records_until(paths, by, |store| {
            draft_member_set(paths, store, &identity, entries, started_ms)
        })?
        .map(|(_, _, drafted)| drafted),
        _ => None,
    };
    let drafted = match drafted {
        Some(drafted) => drafted,
        None => match busy() {
            None => return Ok(MemberWritten::Busy),
            Some(why) => {
                let mut writes = Vec::with_capacity(entries.len());
                for entry in entries {
                    let (read, conflicted) = viewed(paths, &identity, &entry.name);
                    let to = match planned(entry, read.as_ref()) {
                        Ok(to) => to,
                        Err(why) => return Ok(MemberWritten::Refused(why.to_string())),
                    };
                    writes.push(WaitingWrite::member(to, read.as_ref(), conflicted));
                }
                Drafted::Waits(why, writes)
            }
        },
    };
    let waited: Vec<Result<(WaitFor, WaitingWrite), RecordId>> = match drafted {
        Drafted::Refused(why) => return Ok(MemberWritten::Refused(why)),
        Drafted::NotSeated => return Ok(MemberWritten::NotSeated),
        Drafted::Signed(puts) => puts.into_iter().map(Err).collect(),
        Drafted::Waits(why, writes) => {
            let mut landed = Vec::with_capacity(writes.len());
            for (entry, write) in entries.iter().zip(writes) {
                landed.push(Ok((
                    why,
                    land_member_write(
                        paths,
                        &identity,
                        &entry.name,
                        write,
                        Some(std::time::Instant::now() + AUTHOR_OBSERVE),
                    )?,
                )));
            }
            landed
        }
    };
    drop(claim);
    let signed = waited.iter().all(Result::is_err);
    let posted = signed && post_records_to_servers(paths).is_ok_and(|n| n > 0);
    #[cfg(test)]
    BEFORE_OUTCOME.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook();
        }
    });
    let outcomes: Vec<MemberWritten> = entries
        .iter()
        .zip(waited)
        .map(|(entry, waited)| outcome(paths, &identity, &entry.name, waited, posted, started_ms))
        .collect();
    Ok(least_settled(outcomes))
}

/// The least settled of one draft's per-entry outcomes: an overtaken entry first, then one that
/// waits, then one signed here; posted only where every entry is.
fn least_settled(outcomes: Vec<MemberWritten>) -> MemberWritten {
    let rank = |written: &MemberWritten| match written {
        MemberWritten::Overtaken(_) => 0,
        MemberWritten::Waiting(_) => 1,
        MemberWritten::SignedHere => 2,
        _ => 3,
    };
    outcomes
        .into_iter()
        .min_by_key(rank)
        .unwrap_or(MemberWritten::Posted)
}

/// What became of a member write of `name`, read again from the log and the waiting file:
/// `waited` is the write that waits and why, or the op the draft signed (or found holding the
/// value), `posted` whether a server took what it signed, `since_ms` when it began.
fn outcome(
    paths: &Paths,
    identity: &Identity,
    name: &str,
    waited: Result<(WaitFor, WaitingWrite), RecordId>,
    posted: bool,
    since_ms: u64,
) -> MemberWritten {
    let opened = super::super::remove::trusted_store(paths)
        .ok()
        .flatten()
        .and_then(|store| {
            let log = store.log()?.clone();
            let opened = log.open_audience_entries(identity).ok()??;
            Some((log, opened))
        });
    let key = (Audience::Everyone, name.to_owned());
    let winning = opened
        .as_ref()
        .and_then(|(_, opened)| opened.winning_puts.get(&key).copied());
    let overtaken_by = || {
        opened
            .as_ref()
            .and_then(|(log, opened)| overtaker(log, opened, name))
    };
    match waited {
        Err(put) if winning == Some(put) => {
            if posted {
                MemberWritten::Posted
            } else {
                MemberWritten::SignedHere
            }
        }
        Err(_) => MemberWritten::Overtaken(overtaken_by()),
        Ok((why, write)) => {
            if still_waits(paths, identity, name, &write) {
                return MemberWritten::Waiting(why);
            }
            if let Some(dropped) = overtaken_since(paths, name, since_ms) {
                return MemberWritten::Overtaken(dropped.by);
            }
            let read = opened
                .as_ref()
                .and_then(|(_, opened)| opened.by_audience.get(&Audience::Everyone)?.get(name));
            match (read, write.entry()) {
                // A pass signed it since: what it signed goes to the servers now.
                (Some(read), Some(to)) if same_entry(read, to) => {
                    if post_records_to_servers(paths).is_ok_and(|n| n > 0) {
                        MemberWritten::Posted
                    } else {
                        MemberWritten::SignedHere
                    }
                }
                _ => MemberWritten::Overtaken(overtaken_by()),
            }
        }
    }
}

/// What a member's unset did (§3.6), decided by reading the log again afterwards, never from what
/// a pass returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemberUnset {
    /// The `Delete` is signed under a fresh `[everyone]` epoch, and a server took it.
    Posted,
    /// The `Delete` is signed into this Mac's log, and no server took it yet: not yet in vault.
    /// This Mac's next records pass posts what a server lacks.
    SignedHere,
    /// The log holds no value of the name, and this Mac's own write of it, which still waited to
    /// be signed, is taken back: nothing was ever in the vault.
    TookBack,
    /// Neither the log nor this Mac's waiting writes hold the name.
    Absent,
    /// Another write put the name back after the `Delete`: the op that holds it now, where the
    /// log names one.
    Overtaken(Option<Overtaker>),
    /// Refused, signing nothing and taking nothing back: why.
    Refused(String),
    /// A vault pass held this Mac's state past the deadline: nothing is signed.
    Busy,
    /// This Mac has no seat in the vault's log ([`WriteRoute::NotSeated`]).
    NotSeated,
    /// This Mac's vault has no trusted log ([`WriteRoute::NoLog`]): an admin writes it.
    NoLog,
}

/// Why a Mac that does not open every `[everyone]` entry refuses a member's unset (§3.6): the
/// unset seals every entry that stays again under its fresh epoch, and a Mac that cannot open
/// one would leave it out of the vault for every Mac admitted afterwards.
pub const RESEAL_REFUSAL: &str = "an unset seals every entry that stays again under a new vault key, and this Mac does not open every entry: run it on a Mac that opens every entry";

/// Whether the log, as `opened` reads it, holds a value of `name` in `[everyone]` that a member's
/// unset deletes (§3.6): `Ok(true)` where it does (a winning value, or concurrent values where a
/// `Delete` won), `Ok(false)` where it holds none, and the refusal where a Hosts audience this Mac
/// opens holds the name too, or where this Mac cannot seal every `[everyone]` entry again
/// (`OpenedAudienceEntries::reputs_whole`, [`RESEAL_REFUSAL`]).
fn judge_unset(
    opened: &crate::membership_log::entries::OpenedAudienceEntries,
    name: &str,
) -> Result<bool, String> {
    for (audience, entries) in &opened.by_audience {
        if matches!(audience, Audience::Hosts(_)) && entries.contains_key(name) {
            return Err(hosts_refusal(name, audience, true));
        }
    }
    // A vault with no `[everyone]` epoch holds nothing there.
    if !opened.maximal_epochs.contains_key(&EpochAudience::Everyone) {
        return Ok(false);
    }
    if !opened.reputs_whole(&EpochAudience::Everyone) {
        return Err(RESEAL_REFUSAL.to_owned());
    }
    Ok(opened
        .by_audience
        .get(&Audience::Everyone)
        .is_some_and(|entries| entries.contains_key(name))
        || opened
            .conflicts
            .contains(&(Audience::Everyone, name.to_owned())))
}

/// Why a member's unset of `names` is refused where an entry that stays is anchored to one of
/// them (§3.6, as an admin's edit refuses it in `text::check_anchors`): every Mac's view drops an
/// entry whose anchor is no live if-absent entry, so the unset would take that entry off every
/// Mac too. An `[everyone]` entry the unset removes itself, `names` holding it, is no such entry;
/// a Hosts entry this Mac opens is, since its hosts read the anchor from `[everyone]` (a Hosts
/// audience that held the anchor too is refused before, [`judge_unset`]). The refusal names every
/// entry anchored to the first anchor that has one, and what removes them: a member unsets a
/// shared one first; any other is removed with its anchor by an admin's edit. `None` where no
/// entry is anchored to `names`.
fn anchored_refusal(
    opened: &crate::membership_log::entries::OpenedAudienceEntries,
    names: &[&str],
) -> Option<String> {
    let anchored: Vec<(&Audience, &Entry)> = opened
        .by_audience
        .iter()
        .flat_map(|(audience, entries)| entries.values().map(move |entry| (audience, entry)))
        .filter(|(audience, entry)| {
            !(**audience == Audience::Everyone && names.contains(&entry.name.as_str()))
                && entry
                    .anchor
                    .as_deref()
                    .is_some_and(|anchor| names.contains(&anchor))
        })
        .collect();
    let anchor = anchored.first()?.1.anchor.as_deref()?;
    let of_it: Vec<(&Audience, &Entry)> = anchored
        .into_iter()
        .filter(|(_, entry)| entry.anchor.as_deref() == Some(anchor))
        .collect();
    let listed: Vec<String> = of_it
        .iter()
        .map(|(audience, entry)| match audience {
            Audience::Everyone => format!("`{}`", entry.name),
            Audience::Hosts(_) => format!("`{}` (for {})", entry.name, audience.header()),
        })
        .collect();
    let (them, are) = match listed.as_slice() {
        [one] => (one.clone(), "is"),
        _ => (listed.join(", "), "are"),
    };
    let members_unset = of_it.iter().all(|(audience, entry)| {
        **audience == Audience::Everyone
            && crate::entry_class::member_refusal(&entry.name).is_none()
    });
    let remedy = if members_unset {
        format!("unset {them} first")
    } else {
        "no member write removes what is anchored to it: an admin's edit removes the whole set"
            .to_owned()
    };
    Some(format!(
        "{them} {are} anchored to `{anchor}`, and every Mac drops an entry whose anchor is gone, \
         so this unset would remove {} from every Mac too; {remedy}",
        if listed.len() == 1 { "it" } else { "them" }
    ))
}

/// What the draft of a member's unset made.
enum DraftedUnset {
    /// The `Delete`, under a fresh epoch, and the reseal of what stays.
    Deleted,
    /// Nothing signed: the log holds no value of the name.
    Absent,
    /// Nothing signed: refused.
    Refused(String),
    /// Nothing signed: the draft found no seat.
    NotSeated,
}

#[cfg(test)]
thread_local! {
    /// The negative control of [`member_unset`]'s fresh epoch: on, the `Delete` is signed under
    /// the epoch a writer seals under ([`everyone_seal`]), as a plain `Delete` is, and nothing is
    /// sealed again, so the epoch that sealed the removed value stays newest and winning.
    pub(in super::super) static UNSET_UNDER_OLD_EPOCH: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Whether [`UNSET_UNDER_OLD_EPOCH`] is on; never outside a test.
fn unset_under_old_epoch() -> bool {
    #[cfg(test)]
    {
        UNSET_UNDER_OLD_EPOCH.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        false
    }
}

/// The member draft of an unset of `names` (§3.6), all in one draft, against `store`: refused or
/// absent as [`judge_unset`] and [`anchored_refusal`] read the store; else a fresh `[everyone]`
/// epoch wrapped to the Macs a fresh epoch goes to ([`everyone_fresh_to`]), every other
/// `[everyone]` entry sealed again under it, and the `Delete` of each name the log holds beside
/// the `Delete`s a reseal issues again (F21 rule 5,
/// [`crate::membership_log::OpenedAudienceEntries::keyed_deletes`]): the one batch an admin's
/// edit that takes names out of `[everyone]` signs ([`draft_sealed_away`]).
fn draft_member_unset(
    paths: &Paths,
    store: &Store,
    identity: &Identity,
    names: &[&str],
    now_ms: u64,
) -> crate::Result<(Vec<Record>, DraftedUnset)> {
    let nothing = |drafted| Ok((Vec::new(), drafted));
    let Some(log) = store.log() else {
        return nothing(DraftedUnset::NotSeated);
    };
    let Some((actor, device, incarnation)) = open_local_member_signer(paths, log)? else {
        return nothing(DraftedUnset::NotSeated);
    };
    let Some(opened) = log.open_audience_entries(identity)? else {
        return nothing(DraftedUnset::Refused(RESEAL_REFUSAL.to_owned()));
    };
    let mut deleted: Vec<String> = Vec::new();
    for name in names {
        match judge_unset(&opened, name) {
            Err(why) => return nothing(DraftedUnset::Refused(why)),
            Ok(true) => deleted.push((*name).to_owned()),
            Ok(false) => {}
        }
    }
    if let Some(why) = anchored_refusal(&opened, names) {
        return nothing(DraftedUnset::Refused(why));
    }
    if deleted.is_empty() {
        return nothing(DraftedUnset::Absent);
    }
    let mut log = log.clone();
    if unset_under_old_epoch() {
        let seal = everyone_seal(&log, &opened, identity)
            .ok_or_else(|| Error::new("no [everyone] epoch to seal under"))?;
        let deletes: Vec<&String> = deleted.iter().collect();
        let minted = sign_everyone(
            &mut log,
            &opened,
            (&actor, &device, incarnation),
            now_ms,
            seal,
            &deletes,
            &[],
        )?;
        return Ok((minted, DraftedUnset::Deleted));
    }
    let stays: Vec<&Entry> = opened
        .by_audience
        .get(&Audience::Everyone)
        .into_iter()
        .flat_map(BTreeMap::values)
        .filter(|entry| !names.contains(&entry.name.as_str()))
        .collect();
    let dropped: Vec<&String> = deleted
        .iter()
        .chain(
            opened
                .keyed_deletes
                .get(&Audience::Everyone)
                .into_iter()
                .flatten()
                .filter(|again| !deleted.contains(again)),
        )
        .collect();
    let generation = opened
        .max_generation
        .get(&EpochAudience::Everyone)
        .copied()
        .unwrap_or(0)
        .saturating_add(1)
        .max(1);
    let recipients = everyone_fresh_to(&log, &opened);
    let mut minted = Vec::new();
    device.with_seed(|writer_seed| {
        draft_sealed_away(
            &mut log,
            &actor,
            writer_seed,
            incarnation,
            now_ms,
            &Audience::Everyone,
            generation,
            &recipients,
            &stays,
            &dropped,
            &mut minted,
        )
    })?;
    Ok((minted, DraftedUnset::Deleted))
}

/// Remove `name` from `[everyone]` as this Mac, a seated member (§3.6), with no admin key and no
/// envelope; only a `Shared` name ([`crate::entry_class::member_refusal`]). An unset never waits:
/// under the draft claim ([`claim_draft`]) it takes back this Mac's own writes of the name that
/// are still to be signed, so none comes back after the `Delete`, then drafts under
/// [`author_and_commit_records_until`] the `Delete` under a fresh `[everyone]` epoch, with every
/// entry that stays sealed again under it ([`draft_member_unset`]), so no Mac admitted afterwards
/// can open the removed value (F15). It lets the claim go, posts what it signed to every server
/// this Mac reaches, and decides the outcome by reading the log again. Before it takes anything
/// back, it judges the store as the draft will ([`judge_unset`], [`anchored_refusal`]): a refusal
/// there signs nothing and takes nothing back. A refusal the draft alone meets (the store moved in
/// between) signs nothing, and the person's own waiting write of the name stays taken back, as the
/// unset they asked for leaves it. It applies nothing to this Mac's files: its caller syncs where
/// it wants them.
///
/// # Errors
/// This Mac's state or records cannot be read or written, a record fails to sign or keep, or the
/// waiting writes cannot be read or written.
pub fn member_unset(
    paths: &Paths,
    name: &str,
    by: std::time::Instant,
) -> crate::Result<MemberUnset> {
    if let Some(why) = crate::entry_class::member_refusal(name) {
        return Ok(MemberUnset::Refused(why));
    }
    member_unset_names(paths, &[name], by)
}

/// [`member_unset`] of `names`, whose names the caller judged ([`crate::entry_class`]), in one
/// draft under one fresh epoch (`claude_token::remove`'s pair). It is [`MemberUnset::Absent`]
/// only where the log and this Mac's waiting writes hold none of them, and overtaken where the
/// log holds any again afterwards.
///
/// # Errors
/// As [`member_unset`].
pub(crate) fn member_unset_names(
    paths: &Paths,
    names: &[&str],
    by: std::time::Instant,
) -> crate::Result<MemberUnset> {
    match write_route(paths, by)? {
        WriteRoute::NoLog => return Ok(MemberUnset::NoLog),
        WriteRoute::NotSeated => return Ok(MemberUnset::NotSeated),
        WriteRoute::Busy => return Ok(MemberUnset::Busy),
        WriteRoute::Seated => {}
    }
    let identity = super::super::read_identity(paths)?
        .ok_or_else(|| Error::new("this Mac has no secrets identity"))?;
    let past = |error: Error| {
        if std::time::Instant::now() >= by {
            Ok(MemberUnset::Busy)
        } else {
            Err(error)
        }
    };
    let claim = match claim_draft(paths, Some(by)) {
        Ok(claim) => claim,
        Err(error) => return past(error),
    };
    let viewed = super::super::remove::trusted_store(paths)?;
    let Some(log) = viewed.as_ref().and_then(Store::log) else {
        return Ok(MemberUnset::NotSeated);
    };
    let Some(opened) = log.open_audience_entries(&identity)? else {
        return Ok(MemberUnset::Refused(RESEAL_REFUSAL.to_owned()));
    };
    for name in names {
        if let Err(why) = judge_unset(&opened, name) {
            return Ok(MemberUnset::Refused(why));
        }
    }
    if let Some(why) = anchored_refusal(&opened, names) {
        return Ok(MemberUnset::Refused(why));
    }
    let mut took_back = false;
    for name in names {
        match take_back_member_write(paths, &identity, name, Some(by)) {
            Ok(took) => took_back |= took,
            Err(error) => return past(error),
        }
    }
    let started_ms = aterm_time::unix_millis().unwrap_or(0);
    let drafted = author_and_commit_records_until(paths, by, |store| {
        draft_member_unset(paths, store, &identity, names, started_ms)
    })?;
    drop(claim);
    let Some((_, _, drafted)) = drafted else {
        return Ok(MemberUnset::Busy);
    };
    match drafted {
        DraftedUnset::Refused(why) => return Ok(MemberUnset::Refused(why)),
        DraftedUnset::NotSeated => return Ok(MemberUnset::NotSeated),
        DraftedUnset::Absent if took_back => return Ok(MemberUnset::TookBack),
        DraftedUnset::Absent => return Ok(MemberUnset::Absent),
        DraftedUnset::Deleted => {}
    }
    let posted = post_records_to_servers(paths).is_ok_and(|n| n > 0);
    #[cfg(test)]
    BEFORE_OUTCOME.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook();
        }
    });
    let read = super::super::remove::trusted_store(paths)
        .ok()
        .flatten()
        .and_then(|store| {
            let log = store.log()?.clone();
            let opened = log.open_audience_entries(&identity).ok()??;
            Some((log, opened))
        });
    if let Some((log, opened)) = &read
        && let Some(back) = names.iter().find(|name| {
            opened
                .by_audience
                .get(&Audience::Everyone)
                .is_some_and(|entries| entries.contains_key(**name))
        })
    {
        return Ok(MemberUnset::Overtaken(overtaker(log, opened, back)));
    }
    Ok(if posted {
        MemberUnset::Posted
    } else {
        MemberUnset::SignedHere
    })
}

/// How many names a Hosts audience this Mac opens holds that `[everyone]` holds too: the per-host
/// copies that shadow `[everyone]` on this Mac, which wins there. Only a host Mac can count them,
/// since a Hosts audience's names are sealed to its hosts. Value-free; zero where this Mac holds
/// no trusted log or opens no epoch.
///
/// # Errors
/// The configuration, the records or this Mac's identity cannot be read.
pub fn per_host_shadows(paths: &Paths) -> crate::Result<usize> {
    let Some(store) = super::super::remove::trusted_store(paths)? else {
        return Ok(0);
    };
    let Some(log) = store.log() else {
        return Ok(0);
    };
    let Some(identity) = super::super::read_identity(paths)? else {
        return Ok(0);
    };
    let Some(opened) = log.open_audience_entries(&identity)? else {
        return Ok(0);
    };
    let Some(everyone) = opened.by_audience.get(&Audience::Everyone) else {
        return Ok(0);
    };
    let shadowed: BTreeSet<&String> = opened
        .by_audience
        .iter()
        .filter(|(audience, _)| matches!(audience, Audience::Hosts(_)))
        .flat_map(|(_, entries)| entries.keys())
        .filter(|name| everyone.contains_key(*name))
        .collect();
    Ok(shadowed.len())
}
