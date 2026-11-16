// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This machine's name (`docs/DESIGN-machine-names-2026-11-04.md`): its NAME (`m8`, or `unnumbered`
//! while unnumbered), nickname and earlier numbers, kept in `machine-name.toml` whether or not it
//! is in a fleet ([`file`]), made at first run by [`ensure`], and, once a number is chosen,
//! published to its other readers ([`publish`]). A rename's nickname is written to this Mac's
//! lab registry row too ([`registry`]), and a nickname written to that row from any Mac becomes
//! this Mac's at its next read that shows it ([`follow_registry`]).

pub mod file;
pub mod publish;
pub mod registry;

use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::Path;

use aterm_net::fleet_member::fleet_seed;
use aterm_net::fleet_member::name_record::{
    By, ByKind, NickClass, ZERO_DIGEST, nicknames_in_books, node_of, node_word,
};
use aterm_net::fleet_member::{
    NAME_FILE, NAME_FILE_MAX, PrivateFileError, node_identity, read_node_identity,
    read_primary_name, read_private,
};
use aterm_secrets::client;
use aterm_types::machine_name::{
    Class, Entry, Fleet, IMPLIED_SINCE, Kept, Nickname, NodeId, Number, Record, Settled, Verdict,
    check, renamed, settle,
};

use file::{Change, MachineName, Migration, Why};

use crate::members_cli::Standing;

/// Whether this Mac is in a fleet, as the machine title's row and `aterm name`'s `fleet=` say it
/// (`docs/DESIGN-machine-names-2026-11-04.md` §5.2): its vault's membership standing
/// ([`Standing`]), with a shared-secret fleet's key standing for membership until the one-mesh
/// design's D2 deletes the key. A retained key stands for active membership only while its
/// custody admits traffic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InFleet {
    /// A member, or a holder of a readable fleet key with admitted custody.
    In,
    /// On its way in: let in and not yet recorded, asking, or let in with no seat yet.
    Joining,
    /// Never in, taken out, or let go.
    Out,
    /// A retained fleet key whose custody does not admit traffic: why.
    Paused(String),
    /// The vault's local state could not be read: why.
    Unread(String),
}

impl InFleet {
    /// What `standing` says, where `admitted_key` means a readable key with admitted custody
    /// and `revoked` means the vault let this Mac go or took it out ([`client::Membership::Revoked`]), which outranks
    /// a fleet key: the key it still holds is a secret it was let go from, not a membership.
    #[must_use]
    pub fn of(admitted_key: bool, revoked: bool, standing: &Standing) -> Self {
        if revoked || matches!(standing, Standing::Dropped(_)) {
            return Self::Out;
        }
        if admitted_key {
            return Self::In;
        }
        match standing {
            Standing::Member(_) => Self::In,
            Standing::Seatless | Standing::Allowed { .. } | Standing::Asking(_) => Self::Joining,
            Standing::NotIn | Standing::Dropped(_) => Self::Out,
            Standing::Unknown(why) => Self::Unread(why.clone()),
        }
    }

    /// This Mac's, with its fleet key in `state` and its vault at `vault`
    /// ([`Standing::membership_of`], which spawns nothing), or the key alone where no vault is
    /// read ([`standing_vault`]). Revocation is read before the key decides membership; only
    /// a keyless, nonrevoked Mac needs its standing read too. A retained key with custody that
    /// refuses traffic is paused until an explicit take restores custody.
    #[must_use]
    pub fn read(state: &Path, vault: Option<&client::Paths>) -> Self {
        let fleet_key = aterm_net::fleet::read_key_file(state).is_ok();
        let Some(paths) = vault else {
            return Self::of(fleet_key, false, &Standing::NotIn);
        };
        let status = client::status(paths);
        let revoked = matches!(
            &status,
            Ok(client::Status {
                membership: client::Membership::Revoked { .. },
                ..
            })
        );
        if revoked {
            return Self::Out;
        }
        if fleet_key {
            return match crate::fleet_vault::tenure(state, vault).void() {
                Some(why) => Self::Paused(why),
                None => Self::In,
            };
        }
        Self::of(false, false, &Standing::membership_of(paths, status))
    }

    /// `aterm name`'s `fleet=` word.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Joining => "joining",
            Self::Out => "out",
            Self::Paused(_) => "paused",
            Self::Unread(_) => "unread",
        }
    }

    /// The machine menu's words: `in a fleet`, `joining a fleet`, `not in a fleet`,
    /// `fleet paused`, or `fleet not read`.
    #[must_use]
    pub const fn phrase(&self) -> &'static str {
        match self {
            Self::In => "in a fleet",
            Self::Joining => "joining a fleet",
            Self::Out => "not in a fleet",
            Self::Paused(_) => "fleet paused",
            Self::Unread(_) => "fleet not read",
        }
    }
}

/// Where the name [`ensure`] answers came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// `machine-name.toml` was there, or another first reader wrote it first.
    Kept,
    /// This Mac's bound registry machine row supplied its first local name.
    Registry,
    /// Legacy evidence: the roster label's NAME, else an older build's `fleet-name` NAME,
    /// with a word-valued `fleet-name` as the nickname.
    Evidence,
    /// None of these: `unnumbered` and a drawn noun (§2.4).
    Drawn,
}

/// A first local name projected from this Mac's bound registry machine row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryName {
    /// The registry row's canonical machine number.
    pub num: Number,
    /// The nickname and its automatic or chosen provenance; `None` draws a local noun.
    pub nick: Option<(Nickname, NickClass)>,
}

/// Whether the registry can supply the first local name without guessing a binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryNameRead {
    /// No live registry machine row binds this Mac; existing fallback sources apply.
    Unbound,
    /// The registry may name this Mac but its projection is unavailable.
    Pending,
    /// A live row binds this Mac; its name is either valid or unavailable for the stated reason.
    Bound(Result<RegistryName, String>),
}

/// This machine's name, made when no `machine-name.toml` exists in `state` (§3.3). Evidence is
/// read before taking the name lock, then [`file::read_or_create`] publishes the candidate only
/// if no other reader created the file first. Every reader returns the winner; a name made here
/// starts its History with the first name ([`Why::First`]).
///
/// A valid bound registry machine row supplies the name first, as a seeded local mirror with
/// no pending book migration. Its nickname retains its provenance, or a null nickname draws a
/// local automatic noun. A pending registry projection or invalid bound name stops creation.
/// Without a binding, the roster's bare NAME wins, then an older `fleet-name`'s bare NAME,
/// else `unnumbered` and a noun `in_use` does not name, drawn. A word-valued `fleet-name` supplies its
/// chosen nickname. Legacy numbering is seeded at the implied rank with migration pending.
/// Signed book records never restore or replace this local mirror. A `fleet-name` whose text
/// is no name is no evidence; one that cannot be
/// read, a node key that cannot be read and a roster that cannot be read stop creation, so no
/// passing failure is kept as `unnumbered` and the next reader tries again (§3.3 of
/// `docs/DESIGN-machine-names-2026-11-04.md`).
///
/// # Errors
/// `machine-name.toml` cannot be read or written; or, while no other reader supplies the name,
/// the registry name, `fleet-name`, the node key, `roster` or the CSPRNG cannot be read,
/// or every default noun is already in use.
pub fn ensure(
    state: &Path,
    roster: impl FnOnce() -> io::Result<Option<String>>,
    in_use: impl Fn(&str) -> bool,
    now_ms: u64,
) -> io::Result<(MachineName, Origin)> {
    ensure_from(
        state,
        || {
            if aterm_types::dirs::runs_under_private_state_root()
                || aterm_types::dirs::state_dir().as_deref() != Some(state)
            {
                RegistryNameRead::Unbound
            } else {
                crate::harness::alab_roster::machine_name()
            }
        },
        roster,
        in_use,
        now_ms,
    )
}

/// Creation over injected registry and legacy sources. Preparing a candidate happens before
/// the name lock; a concurrent winner supersedes either that candidate or its preparation error.
fn ensure_from(
    state: &Path,
    registry: impl FnOnce() -> RegistryNameRead,
    roster: impl FnOnce() -> io::Result<Option<String>>,
    in_use: impl Fn(&str) -> bool,
    now_ms: u64,
) -> io::Result<(MachineName, Origin)> {
    if let Some(name) = file::read(state)? {
        return Ok((name, Origin::Kept));
    }
    // Preparing a first name can read the vault's roster. Do that before taking the name lock:
    // fleet writers acquire vault before book, and edits acquire book before name.
    let prepared = match registry() {
        RegistryNameRead::Unbound => first(state, roster, in_use, now_ms),
        RegistryNameRead::Pending => Err(io::Error::other(
            "the lab registry's machine name is not available yet",
        )),
        RegistryNameRead::Bound(Err(why)) => Err(io::Error::other(format!(
            "the registry machine name is unavailable: {why}"
        ))),
        RegistryNameRead::Bound(Ok(candidate)) => first_registry(candidate, in_use, now_ms),
    };
    let mut origin = Origin::Kept;
    let (name, created) = file::read_or_create(state, || {
        let (mut candidate, source) = prepared?;
        origin = source;
        let first = Change::first(&candidate, now_ms);
        candidate.record(first);
        Ok(candidate)
    })?;
    Ok((name, if created { origin } else { Origin::Kept }))
}

/// Take the nickname a person or a program wrote to this Mac's own `lab/machines` row from any
/// Mac (the Tables page, `aterm file set lab/machines <row> nickname <word>`) into the name kept
/// in `state` (tracker package FL-169), as a read that shows the name makes it (the menu bar's
/// title, `aterm name`): a live snapshot must match this Mac's own binding and number, and the
/// current native source is admitted through the atomic name write. Retained display rows
/// confer no naming authority. The name writer stays held through admission and persistence.
/// The registry keeps who wrote the row and when; History records the change as
/// [`Why::Registry`], with no author. An automatic write and an empty or conflicted cell are not
/// followed, and a name whose row is none of this Mac's ([`registry::row_nick`]) is left as it
/// is. Answers the name where the file changed.
///
/// # Errors
/// The file cannot be read or replaced, its lock is held, or its native source refuses admission.
pub fn follow_registry(state: &Path, now_ms: u64) -> io::Result<Option<MachineName>> {
    let Some(kept) = file::read(state)? else {
        return Ok(None);
    };
    // Retained data may avoid work, but never authorize the write below.
    if row_followed(state, &kept, now_ms).is_none() {
        return Ok(None);
    }
    let Some(row) = registry::for_follow(state) else {
        return Ok(None);
    };
    if !row.names(kept.num) || followed(&kept, row.nickname.as_ref(), now_ms).is_none() {
        return Ok(None);
    }
    let mut writer = file::writer(state)?;
    if !row.names(writer.name().num) {
        return Ok(None);
    }
    let Some(next) = followed(writer.name(), row.nickname.as_ref(), now_ms) else {
        return Ok(None);
    };
    row.with_admitted(|| writer.replace(next.clone()))?;
    Ok(Some(next))
}

/// `held` once it has seen this Mac's row as `state`'s name reads it now ([`followed`]), or
/// `None` where nothing moves.
fn row_followed(state: &Path, held: &MachineName, now_ms: u64) -> Option<MachineName> {
    registry::row_nick(state).and_then(|row| followed(held, row.as_ref(), now_ms))
}

/// `held` once it has seen its registry row's chosen nickname `row` (`None`: none chosen), or
/// `None` where it saw that already ([`file::MachineName::row`]). A chosen nickname other than
/// `held`'s renames it ([`renicked`], with no author, recorded as [`Why::Registry`]) where the
/// file saw the row before, and where it never did and its own nickname is a drawn noun, which
/// nobody chose. A first look at a row from a file whose nickname a person or a program chose
/// (one an earlier build wrote, or one made while no row was this Mac's) only marks it seen, so
/// a row value older than that choice does not replace it; so does a row holding `held`'s own
/// nickname (a rename's own write).
fn followed(held: &MachineName, row: Option<&Nickname>, now_ms: u64) -> Option<MachineName> {
    let seen = file::RowSeen::Saw(row.cloned());
    if held.row == seen {
        return None;
    }
    let looked = held.row != file::RowSeen::Never || held.nick_class == NickClass::Auto;
    let mut next = match row {
        Some(row) if looked && held.nick != *row => renicked(held, row.clone(), None).map_or_else(
            || held.clone(),
            |mut next| {
                let change = Change::between(held, &next, now_ms, Why::Registry);
                next.record(change);
                next
            },
        ),
        _ => held.clone(),
    };
    next.row = seen;
    Some(next)
}

fn first_registry(
    candidate: RegistryName,
    in_use: impl Fn(&str) -> bool,
    now_ms: u64,
) -> io::Result<(MachineName, Origin)> {
    let row = match &candidate.nick {
        Some((nick, NickClass::Chosen)) => Some(nick.clone()),
        _ => None,
    };
    let (nick, nick_class) = match candidate.nick {
        Some(nick) => nick,
        None => (file::draw_nick(in_use)?, NickClass::Auto),
    };
    let mut name = MachineName::unnumbered(nick, now_ms);
    name.row = file::RowSeen::Saw(row);
    name.num = Some(candidate.num);
    name.class = Class::Seeded;
    name.since = IMPLIED_SINCE;
    name.nick_class = nick_class;
    Ok((name, Origin::Registry))
}

/// Creation from an injected registry snapshot for composition tests and private debug drives.
/// It uses the same precedence, atomic replacement and concurrent-winner handling as [`ensure`].
///
/// # Errors
/// As [`ensure`].
#[cfg(any(test, debug_assertions))]
pub fn ensure_from_registry_for_test(
    state: &Path,
    registry: impl FnOnce() -> RegistryNameRead,
    roster: impl FnOnce() -> io::Result<Option<String>>,
    in_use: impl Fn(&str) -> bool,
    now_ms: u64,
) -> io::Result<(MachineName, Origin)> {
    ensure_from(state, registry, roster, in_use, now_ms)
}

/// [`ensure`] as a reader on this machine runs it, which `aterm name` and the menu bar share:
/// `roster` read only during first-name fallback when no registry row binds this Mac, and a noun redrawn while
/// another node's record in this node's books carries it ([`taken_in`], read only once a noun is
/// drawn).
///
/// # Errors
/// As [`ensure`].
pub fn ensure_in_view(
    state: &Path,
    roster: impl FnOnce() -> io::Result<Option<String>>,
    now_ms: u64,
) -> io::Result<(MachineName, Origin)> {
    let taken = OnceCell::new();
    let in_use = |word: &str| {
        in_view(
            taken.get_or_init(|| {
                // Read only once a noun is drawn, after `first` has read the node key and stopped
                // on its error, so a kept name reads whatever the node key's state.
                let me = read_node_identity(state).ok().flatten();
                taken_in(state, me.as_ref().map(|me| me.node()))
            }),
            word,
        )
    };
    ensure(state, roster, in_use, now_ms)
}

/// Where a nickname change takes its nickname from (§5.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NickTo {
    /// This word.
    Word(Nickname),
    /// A noun drawn again, past the nicknames in view and the one held (§2.4).
    Random,
}

/// What a nickname change did ([`set_nick`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NickSet {
    /// The name as it now stands, or would stand after a dry run.
    pub name: MachineName,
    /// Where the local name came from: made by the first read, or kept.
    pub origin: Origin,
    /// The nickname or its chosen provenance changed.
    pub changed: bool,
    /// Another node's record in this node's books carries the nickname as it now stands.
    /// [`set_nick`] refuses a nickname another node carries (§5.4 of
    /// `docs/DESIGN-machine-names-2026-11-04.md`), so a nickname it sets reads `false` here.
    pub shared: bool,
}

/// The number to choose, optionally choosing a nickname in the same atomic change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameTo {
    /// The machine's number.
    pub num: Number,
    /// A chosen nickname; `None` keeps the nickname it already has.
    pub nick: Option<Nickname>,
}

/// What a number and optional nickname change did ([`set_name`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameSet {
    /// The name as it now stands, or would stand after a dry run.
    pub name: MachineName,
    /// Where the local name came from: made by the first read, or kept.
    pub origin: Origin,
    /// A requested field changed in the local file.
    pub changed: bool,
    /// Another node in view carries the nickname this change kept (one it chose is refused
    /// where another node carries it, §5.4).
    pub shared: bool,
}

/// Choose this machine's number and optionally its nickname, atomically under the name lock.
/// The book lock is held first so a signed claim cannot arrive between the conflict check and
/// the write. The local file is the edit's sole base; its own older signed records cannot
/// replace it. A number held by another signed node is refused until the registry's conflict
/// resolution can move that node. So is a
/// number the vault holds for another Mac (`lab`, read before any lock is taken:
/// [`lab_labels`], its roster and log seats, which other Macs reach a Mac by on today's
/// lab, §5.3 of `docs/DESIGN-machine-names-2026-11-04.md`), and a move off the number the
/// policy names this Mac's vault server by, which other Macs dial as `m<N>.local` and which the
/// macOS names follow while identity naming is on. `lab` is not read for a request that keeps the
/// stored number, a nickname change typed with it; should a concurrent local edit change that
/// number before the name lock is taken, the request is refused rather than checked unread.
/// The earlier local number is kept in `was` for aliases, and the change in History
/// ([`Why::Named`]), which **Undo** reads ([`undo_last`]). A dry run makes no node key and
/// changes no existing name, though its first read makes the name as any first read does.
///
/// # Errors
/// As [`ensure_in_view`]; `lab` fails; the node, seed or books cannot be read, a signed node or
/// the vault holds the number, the move would leave the policy's server name, the stored number
/// moved under the lock while `lab` was unread, another node holds the chosen nickname,
/// a lock cannot be taken, or the changed name cannot be written. Each refusal of the number or
/// nickname, and an unread `lab`, carries its [`Refused`] ([`Refused::of`]).
pub fn set_name(
    state: &Path,
    to: &NameTo,
    lab: impl FnOnce() -> io::Result<client::LabLabels>,
    kind: ByKind,
    roster: impl FnOnce() -> io::Result<Option<String>>,
    now_ms: u64,
    dry_run: bool,
) -> io::Result<NameSet> {
    let (_, origin) = ensure_in_view(state, roster, now_ms)?;
    let by = editor(state, kind, dry_run)?;
    // Whether the request keeps the stored number, as the lock below reads it again.
    let keeps = file::read(state)?.is_some_and(|name| !moves(&name, to.num));
    let lab = if keeps { None } else { Some(read_lab(lab)?) };
    edit_name(state, by, origin, now_ms, dry_run, |held, locked| {
        change_claim(held, to, lab.as_ref(), locked, by, now_ms, Why::Named)
    })
}

/// Who makes an edit for `kind`: this node, its key minted where it has none, or for a dry run
/// only read, so a keyless dry run answers `None` and mints nothing.
fn editor(state: &Path, kind: ByKind, dry_run: bool) -> io::Result<Option<By>> {
    let me = if dry_run {
        read_node_identity(state)?
    } else {
        Some(node_identity(state)?)
    };
    me.as_ref()
        .map(|me| {
            node_of(me.node())
                .map(|node| By { node, kind })
                .ok_or_else(|| io::Error::other("this machine has no valid node id"))
        })
        .transpose()
}

/// The vault's labels `lab` reads, an unread set refused as [`Cause::Unread`].
fn read_lab(lab: impl FnOnce() -> io::Result<client::LabLabels>) -> io::Result<client::LabLabels> {
    lab().map_err(|why| Refused::new(Cause::Unread, why.to_string()).into())
}

/// What the book lock shows an edit of this machine's name: this node, each node's newest
/// signed claim in its books, and the nicknames other nodes' records carry.
struct Locked {
    node: NodeId,
    view: BTreeMap<NodeId, Entry>,
    taken: Vec<Nickname>,
}

/// Edit this machine's name under the book lock and then the name lock, so no signed claim
/// arrives between an edit's checks and its write: `edit` answers the name the stored one
/// becomes, from what the books show ([`Locked`]), once the stored one has followed this Mac's
/// registry row ([`followed`]), and the file is replaced with it unless `dry_run`. `by` names this node; a keyless dry run's `None` stands for a node absent from the
/// books ([`absent_node`]).
fn edit_name(
    state: &Path,
    by: Option<By>,
    origin: Origin,
    now_ms: u64,
    dry_run: bool,
    edit: impl FnOnce(&MachineName, &mut Locked) -> io::Result<MachineName>,
) -> io::Result<NameSet> {
    aterm_net::fleet_member::write_held(state, aterm_net::fleet_member::Began::Here, |_| {
        let books = rename_books(state, now_ms)?;
        let node = match by {
            Some(by) => by.node,
            None => absent_node(&books.view)?,
        };
        let taken = books.taken_except(node);
        let mut locked = Locked {
            node,
            view: books.view,
            taken,
        };
        let mut changed = false;
        let mut preview = None;
        let name = file::replace(state, |stored| {
            // The edit starts from the name as this Mac's registry row leaves it now
            // ([`row_followed`]), so it never writes over a nickname the row was given and no
            // read followed yet, and the row value it replaces counts as seen: the row still
            // holding it until this edit's own write arrives undoes nothing.
            let base = row_followed(state, stored, now_ms);
            let base = base.as_ref().unwrap_or(stored);
            let next = edit(base, &mut locked)?;
            // What the request changed, not what the follow did.
            changed = next != *base;
            if dry_run {
                preview = Some(next);
            } else {
                *stored = next;
            }
            Ok(())
        })?;
        let name = preview.unwrap_or(name);
        let shared = in_view(&locked.taken, name.nick.as_str());
        Ok(NameSet {
            name,
            origin,
            changed,
            shared,
        })
    })
    .map_err(|e| io::Error::other(e.to_string()))?
}

/// `held` with its claim on `to.num`, judged against the vault's labels `lab` (unread only for a
/// request that keeps the number) and the books ([`judge`]): a number that moves makes a new
/// explicit claim ([`renamed`]), which keeps the earlier number in `was`; a nickname `to` names
/// is chosen. A change raises `rev`, names `by` and records one [`Change`] for `why`
/// ([`recorded`]); `held` as it is answers a request that changes nothing.
///
/// # Errors
/// [`judge`] refuses the number, another node's record carries the nickname, or the name clock,
/// take chain or revision is exhausted.
fn change_claim(
    held: &MachineName,
    to: &NameTo,
    lab: Option<&client::LabLabels>,
    locked: &mut Locked,
    by: Option<By>,
    now_ms: u64,
    why: Why,
) -> io::Result<MachineName> {
    let current = current_record(held);
    locked
        .view
        .insert(locked.node, Entry::Record(current.clone()));
    let settled = settle(&locked.view);
    let kept = kept_labels(lab);
    let fleet = Fleet {
        view: &locked.view,
        settled: &settled,
        top: current.top,
        kept: &kept,
        labels: &BTreeMap::new(),
    };
    judge(to.num, held, lab, &fleet, locked.node)?;
    let mut next = held.clone();
    let renumber = current.num != Some(to.num) || current.class != Class::Chosen;
    if renumber {
        let latest = locked
            .view
            .values()
            .filter_map(|entry| match entry {
                Entry::Record(record) => Some(record.act.max(record.since)),
                Entry::Implied(_) | Entry::Unknown(_) => None,
            })
            .max()
            .unwrap_or(0);
        let stamp = now_ms.max(
            latest
                .checked_add(1)
                .ok_or_else(|| io::Error::other("the name clock is exhausted"))?,
        );
        // Choosing a number this record already holds must not surrender its
        // evidence rank to a later rival. The existing take chain makes the
        // explicit claim while preserving that rank, including seeded ranks.
        let take = if current.num == Some(to.num) && !current.waits {
            let generation = current
                .tgen
                .checked_add(1)
                .filter(|generation| *generation < u32::MAX)
                .ok_or_else(|| io::Error::other("the name take chain is exhausted"))?;
            Some((current.since, current.base_or(locked.node), generation))
        } else {
            None
        };
        let claim = renamed(&current, to.num, take, stamp);
        next.num = claim.num;
        next.class = claim.class;
        next.since = claim.since;
        next.prev = claim.prev;
        next.base = claim.base;
        next.tgen = claim.tgen;
        next.sgen = claim.sgen;
        next.act = claim.act;
        next.was = claim.was;
        next.migration = None;
    }
    if let Some(nick) = &to.nick {
        require_free_nick(&locked.taken, nick)?;
        next.nick = nick.clone();
        next.nick_class = NickClass::Chosen;
    }
    recorded(held, next, by, now_ms, why)
}

/// `next`, the name `held` becomes, as a change `by` made at `now_ms` for `why`: `rev` raised,
/// `by` named and the change recorded as History's newest ([`MachineName::record`]); `next`
/// as it is where it equals `held`.
///
/// # Errors
/// The revision is exhausted.
fn recorded(
    held: &MachineName,
    mut next: MachineName,
    by: Option<By>,
    now_ms: u64,
    why: Why,
) -> io::Result<MachineName> {
    if next != *held {
        next.rev = held
            .rev
            .checked_add(1)
            .ok_or_else(|| io::Error::other("the name revision is exhausted"))?;
        next.by = by;
        let change = Change::between(held, &next, now_ms, why);
        next.record(change);
    }
    Ok(next)
}

/// Why a rename is refused: what refused it ([`Cause`]), which tests and the live line read, and
/// its words, which people read. [`judge`] and [`RenamePreview::judge`] answer one; [`set_name`]
/// carries it inside the `io::Error` it fails with, in the same words ([`Refused::of`] finds
/// it there), so the live line and the set refuse a number alike.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    /// What refused it.
    pub cause: Cause,
    /// Its words.
    pub why: String,
}

/// What refused a rename ([`Refused`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cause {
    /// This Mac's stored number moved while the rename was read with the vault's labels unread,
    /// so the number was not checked against them: rename again.
    Moved,
    /// The vault's labels could not be read, so a number that moves this Mac's is unchecked.
    Unread,
    /// The vault's policy names this Mac's server by this number, which other Macs dial as
    /// `<number>.local`: the move would leave it.
    Server(Number),
    /// The vault gives the number to another Mac by this hold (§5.3): its roster label, alias
    /// or sheet claim, its vault `[machines]` name, its membership-log seat, or the roster's
    /// denial.
    Vault(client::LabelHold),
    /// The check gives the number to another node or Mac, where no vault hold does: this
    /// verdict, never [`Verdict::Same`], [`Verdict::Free`] or [`Verdict::EarlierName`].
    Verdict(Verdict),
    /// Another machine's record carries the nickname.
    Nickname,
    /// Undo: History keeps no change to take back, or only the first name, or its newest change
    /// kept both the number and the nickname (a claim of the number this Mac already showed).
    Nothing,
    /// Undo: History's newest change is an undo, or was taken back already. Undo is single-level:
    /// an undo is never undone.
    Undone,
    /// Undo: the name no longer stands as History's newest change left it, so a change History
    /// does not hold came after it.
    Superseded,
    /// Undo: the change gave this Mac its first number, and no rename leaves a Mac unnumbered.
    Unnumbered,
    /// Undo: History's newest change is not the one the undo was asked over, so the name changed
    /// after it was read ([`undo_last`]'s `seen`).
    Unseen,
}

impl Refused {
    const fn new(cause: Cause, why: String) -> Self {
        Self { cause, why }
    }

    /// The check's verdict, where one gave the number to another node or Mac.
    #[must_use]
    pub const fn verdict(&self) -> Option<&Verdict> {
        match &self.cause {
            Cause::Verdict(verdict) => Some(verdict),
            _ => None,
        }
    }

    /// Whether another Mac's claim or hold on this one number refused it, so another number can
    /// pass: the vault's hold ([`Cause::Vault`]), or a verdict that gives the number to another
    /// node or Mac by its claim, label, unconfirmed record, departure or planned row
    /// ([`Verdict::TakesAuto`], [`Verdict::HeldByPerson`], [`Verdict::HeldSeeded`],
    /// [`Verdict::HeldByOlderBuild`], [`Verdict::Labelled`], [`Verdict::Unconfirmed`],
    /// [`Verdict::Retired`], [`Verdict::Planned`]). Every other cause refuses whatever number is
    /// typed (a moved or unread read, the policy's server name, a second node or another account
    /// on this machine), refuses the nickname, or is an undo's or a grammar's, and so is no
    /// number's: `false`.
    #[must_use]
    pub const fn held_by_another(&self) -> bool {
        matches!(
            &self.cause,
            Cause::Vault(_)
                | Cause::Verdict(
                    Verdict::TakesAuto { .. }
                        | Verdict::HeldByPerson { .. }
                        | Verdict::HeldSeeded { .. }
                        | Verdict::HeldByOlderBuild { .. }
                        | Verdict::Labelled { .. }
                        | Verdict::Unconfirmed { .. }
                        | Verdict::Retired { .. }
                        | Verdict::Planned
                )
        )
    }

    /// The refusal `error` carries, where a rename was refused rather than failed.
    #[must_use]
    pub fn of(error: &io::Error) -> Option<&Self> {
        error.get_ref()?.downcast_ref()
    }
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.why)
    }
}

impl std::error::Error for Refused {}

impl From<Refused> for io::Error {
    fn from(refused: Refused) -> Self {
        Self::other(refused)
    }
}

/// Whether selecting `num` makes a new explicit claim: a different number, an automatic or
/// seeded number not chosen yet, or a number a migration still waits on.
fn moves(held: &MachineName, num: Number) -> bool {
    held.num != Some(num)
        || held.class != Class::Chosen
        || held.migration == Some(Migration::Waiting)
}

/// The vault's labels as the pure check keeps them: each number held for another Mac,
/// bound to no node in view.
fn kept_labels(lab: Option<&client::LabLabels>) -> Vec<Kept> {
    lab.iter()
        .flat_map(|lab| lab.held.keys())
        .map(|num| Kept::Label {
            num: *num,
            node: None,
        })
        .collect()
}

/// A node absent from `view`, for an unnumbered, keyless dry run or preview, which needs an
/// identity only for the pure check and makes no persistent key.
fn absent_node(view: &BTreeMap<NodeId, Entry>) -> io::Result<NodeId> {
    let mut id = 0_u128;
    loop {
        let node = NodeId::new(&id.to_le_bytes())
            .ok_or_else(|| io::Error::other("a preview node needs sixteen bytes"))?;
        if !view.contains_key(&node) {
            return Ok(node);
        }
        id = id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("no preview node"))?;
    }
}

/// This node's claim as an edit starting from `held` stands in the view: a number a migration
/// waits on stays the previous one and holds no current claim.
fn current_record(held: &MachineName) -> Record {
    let waits = held.migration == Some(Migration::Waiting);
    Record {
        num: if waits { None } else { held.num },
        class: held.class,
        since: held.since,
        base: held.base,
        tgen: held.tgen,
        sgen: held.sgen,
        act: held.act,
        anchor: None,
        prev: if waits { held.num } else { held.prev },
        waits,
        ephemeral: false,
        was: held.was.clone(),
        top: held.num.map_or(0, Number::get),
        cap: held.cap,
    }
}

/// [`set_name`]'s judgment of a rename to `num`, from the name `held` it starts from, the vault's
/// labels `lab` (unread only for a request that keeps the number) and `fleet`, which holds this
/// node's own claim: the verdict it renames by ([`Verdict::Same`], [`Verdict::Free`] or
/// [`Verdict::EarlierName`]). [`RenamePreview::judge`] shares it, so the live line and the set
/// refuse a number alike.
///
/// # Errors
/// Why the rename is refused: the number moved while `lab` was unread ([`Cause::Moved`]), the
/// move leaves the policy's server name ([`Cause::Server`]), the vault gives the number to
/// another Mac ([`Cause::Vault`], with what holds it), or the check gives it to another node or
/// Mac ([`Cause::Verdict`]).
fn judge(
    num: Number,
    held: &MachineName,
    lab: Option<&client::LabLabels>,
    fleet: &Fleet<'_>,
    node: NodeId,
) -> Result<Verdict, Refused> {
    if moves(held, num) && lab.is_none() {
        return Err(Refused::new(
            Cause::Moved,
            format!(
                "this Mac's number moved while the rename was read, so {num} was not checked \
                 against the vault: rename again"
            ),
        ));
    }
    // The policy dials this Mac's vault server by the name its macOS names follow: its chosen
    // number, else its roster label (`converge::projected_machine_label`).
    if let Some(lab) = lab
        && let Some(dialed) = held
            .num
            .filter(|_| held.class == Class::Chosen)
            .or(lab.label)
            .filter(|dialed| *dialed != num && lab.servers.contains(dialed))
    {
        return Err(Refused::new(
            Cause::Server(dialed),
            format!(
                "the vault's policy names this Mac's server {dialed}, which other Macs dial as \
                 {dialed}.local: name it {num} in the policy's servers first"
            ),
        ));
    }
    let verdict = match check(&num.to_string(), fleet, node) {
        // Same is a no-op for a chosen number, but selecting a seeded/automatic number
        // makes a new claim. Its current lab hold still governs that choice.
        Verdict::Same if moves(held, num) && lab.is_some_and(|lab| lab.held.contains_key(&num)) => {
            Verdict::Labelled { by: None }
        }
        verdict => verdict,
    };
    let why = match &verdict {
        Verdict::Same | Verdict::Free | Verdict::EarlierName { .. } => return Ok(verdict),
        Verdict::TakesAuto { from: by }
        | Verdict::HeldByPerson { by }
        | Verdict::HeldSeeded { by }
        | Verdict::HeldByOlderBuild { by } => format!("{num} already belongs to {by}"),
        Verdict::SecondNode { of } | Verdict::OtherAccountNamed { node: of } => {
            format!("another account on this machine ({of}) holds its number")
        }
        Verdict::Unconfirmed { .. } => format!("{num} is waiting for its holder's name record"),
        Verdict::Labelled { .. } => match lab.and_then(|lab| lab.held.get(&num)) {
            Some(hold) => {
                return Err(Refused::new(
                    Cause::Vault(*hold),
                    format!("{num} is {}", hold.phrase()),
                ));
            }
            None => format!("{num} is another Mac's label in this fleet"),
        },
        _ => format!("{num} is not available in this fleet"),
    };
    Err(Refused::new(Cause::Verdict(verdict), why))
}

/// A rename's inputs as one read found them, for Rename This Mac…'s live line (§5.3 of
/// `docs/DESIGN-machine-names-2026-11-04.md`): [`RenamePreview::judge`] answers each number typed
/// as [`set_name`] would from this read ([`rename_preview`]). The read takes no lock but the one
/// a first read takes to make the name ([`ensure_in_view`]), and makes no node key, so the set
/// that follows reads again under its locks and may still refuse a number the preview passed.
#[derive(Clone, Debug)]
pub struct RenamePreview {
    /// The name the edit starts from: the local file's.
    name: MachineName,
    /// Each node's newest signed claim in this node's books, this node's own included.
    view: BTreeMap<NodeId, Entry>,
    settled: Settled,
    top: u32,
    node: NodeId,
    /// Whether `node` is this Mac's own identity rather than an absent node's stand-in.
    own_node: bool,
    /// The vault's labels, or why they were not read.
    lab: Result<client::LabLabels, String>,
    kept: Vec<Kept>,
    /// The nicknames other nodes' records in this node's books carry.
    taken: Vec<Nickname>,
}

impl RenamePreview {
    /// The name the edit starts from.
    #[must_use]
    pub const fn name(&self) -> &MachineName {
        &self.name
    }

    /// This Mac's node, as the read found its identity; `None` for a Mac with no node key yet,
    /// which the read judges as a node absent from its books.
    #[must_use]
    pub const fn node(&self) -> Option<NodeId> {
        if self.own_node { Some(self.node) } else { None }
    }

    /// What [`set_name`] would answer a rename to `num` from this read: the verdict it renames by
    /// ([`Verdict::Same`], [`Verdict::Free`] or [`Verdict::EarlierName`]).
    ///
    /// # Errors
    /// The refusal [`set_name`] refuses `num` with, and, for a number that moves this Mac's, why
    /// the vault's labels were not read ([`Cause::Unread`]).
    pub fn judge(&self, num: Number) -> Result<Verdict, Refused> {
        // As `set_name`: the vault's labels are read, and judged, only for a number that moves.
        let lab = if moves(&self.name, num) {
            Some(
                self.lab
                    .as_ref()
                    .map_err(|why| Refused::new(Cause::Unread, why.clone()))?,
            )
        } else {
            None
        };
        let fleet = Fleet {
            view: &self.view,
            settled: &self.settled,
            top: self.top,
            kept: if lab.is_some() { &self.kept } else { &[] },
            labels: &BTreeMap::new(),
        };
        judge(num, &self.name, lab, &fleet, self.node)
    }

    /// Whether another node's record carries the same canonical name key.
    #[must_use]
    pub fn shared(&self, nick: &Nickname) -> bool {
        in_view(&self.taken, nick.as_str())
    }

    /// Why the vault's labels were not read, when `num` moves this Mac's number and so needs them:
    /// [`RenamePreview::judge`] then refuses `num` in these words ([`Cause::Unread`]), while
    /// [`set_name`] reads the labels again at the set and may find them.
    #[must_use]
    pub fn unchecked(&self, num: Number) -> Option<&str> {
        self.lab
            .as_ref()
            .err()
            .filter(|_| moves(&self.name, num))
            .map(String::as_str)
    }

    /// Whether this Mac shows a number a migration still waits on, which holds no claim yet: a
    /// rename to it makes the claim, after the same checks as any number that moves.
    #[must_use]
    pub fn waits(&self) -> bool {
        self.name.migration == Some(Migration::Waiting)
    }

    /// What [`undo_last`] would answer from this read: where it takes the name back to
    /// ([`undo_to`]), its number judged as [`set_name`] judges one ([`RenamePreview::judge`]) and
    /// its nickname refused where another node's record carries it.
    ///
    /// # Errors
    /// History holds nothing [`undo_last`] takes back ([`undo_to`]), the number is refused, or
    /// another node's record carries the nickname ([`Cause::Nickname`]).
    pub fn undo(&self) -> Result<UndoTo, Refused> {
        let to = undo_to(&self.name)?;
        let nick = match &to {
            UndoTo::Name(to) => {
                self.judge(to.num)?;
                to.nick.as_ref()
            }
            UndoTo::Nickname(nick) => Some(nick),
        };
        if let Some(nick) = nick.filter(|nick| self.shared(nick)) {
            return Err(nick_used(nick));
        }
        Ok(to)
    }

    /// What Undo Last Rename offers from this read. It is [`RenamePreview::undo`]'s answer,
    /// except that a number the vault's labels were not read for is left for [`undo_last`] to
    /// check. That case answers with why the labels were not read ([`RenamePreview::unchecked`]),
    /// as the live line leaves Save available over unread labels. The nickname is judged either
    /// way.
    ///
    /// # Errors
    /// As [`RenamePreview::undo`], except that a number left unchecked over unread labels is
    /// never refused ([`Cause::Unread`]): History holds nothing [`undo_last`] takes back, the
    /// number is refused, or another node's record carries the nickname ([`Cause::Nickname`]).
    pub fn undo_unchecked(&self) -> Result<(UndoTo, Option<&str>), Refused> {
        let to = undo_to(&self.name)?;
        let why = match &to {
            UndoTo::Name(to) => self.unchecked(to.num),
            UndoTo::Nickname(_) => None,
        };
        let Some(why) = why else {
            return self.undo().map(|to| (to, None));
        };
        if let UndoTo::Name(NameTo {
            nick: Some(nick), ..
        }) = &to
            && self.shared(nick)
        {
            return Err(nick_used(nick));
        }
        Ok((to, Some(why)))
    }

    /// Up to `want` numbers above `num`, lowest first, that a rename from this read takes as
    /// [`Verdict::Free`]: no node holds one, no label keeps one, and no other Mac went by one
    /// earlier, so a person whose number is taken can pick another. It tries at most
    /// [`FREE_SCAN`] numbers, in memory, and finds none where the vault's labels were not read.
    /// A rename's view holds no tombstone and no planned registry row yet, so a number a departed
    /// Mac last held, or one a planned row reserves, is `Free` to a rename and offered here.
    #[must_use]
    pub fn free_after(&self, num: Number, want: usize) -> Vec<Number> {
        let last = num.get().saturating_add(FREE_SCAN).min(Number::MAX);
        (num.get().saturating_add(1)..=last)
            .filter_map(Number::new)
            .filter(|candidate| self.judge(*candidate) == Ok(Verdict::Free))
            .take(want)
            .collect()
    }

    /// The numbers a person whose typed `num` another Mac holds can take instead: the first
    /// [`FREE_SHOWN`] that [`set_name`] takes as [`Verdict::Free`] from this read
    /// ([`RenamePreview::free_after`]), where `refused`, the refusal of `num`, is another Mac's
    /// claim or hold on it ([`Refused::held_by_another`]); none for a refusal that would refuse
    /// any other number too. Rename This Mac…, Settings ▸ This Mac and `aterm name set` name
    /// these ([`next_free_words`]).
    #[must_use]
    pub fn next_free(&self, num: Number, refused: &Refused) -> Vec<Number> {
        if refused.held_by_another() {
            self.free_after(num, FREE_SHOWN)
        } else {
            Vec::new()
        }
    }
}

/// The most numbers [`RenamePreview::free_after`] tries.
pub const FREE_SCAN: u32 = 1_000;

/// How many free numbers a refusal of a held one names ([`RenamePreview::next_free`]).
pub const FREE_SHOWN: usize = 3;

/// The words naming `free` beside a refused number, every surface's: `next free: m8, m9, m10`,
/// lowercase with no full stop as [`Refused::why`] is (the GUI makes a sentence of it, the CLI
/// prints it as it is); `None` where `free` is empty.
#[must_use]
pub fn next_free_words(free: &[Number]) -> Option<String> {
    let (first, rest) = free.split_first()?;
    let mut words = format!("next free: {first}");
    for num in rest {
        words.push_str(", ");
        words.push_str(&num.to_string());
    }
    Some(words)
}

/// Where an undo takes the name back to ([`undo_to`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UndoTo {
    /// Back to this number, as a new explicit claim on it, and to the nickname where the change
    /// replaced that too.
    Name(NameTo),
    /// Back to this nickname, chosen; the number and its claim stay as they are.
    Nickname(Nickname),
}

impl UndoTo {
    /// The title the undo leaves `held`, the name it starts from ([`file::title_of`]): the number
    /// it takes back with the nickname it brings back, or `held`'s nickname as the title shows it
    /// where the change kept that; or `held`'s number with the nickname it brings back.
    #[must_use]
    pub fn title(&self, held: &MachineName) -> String {
        match self {
            Self::Name(to) => file::title_of(
                Some(to.num),
                &to.nick
                    .as_ref()
                    .map_or_else(|| held.display_nick(), |nick| nick.as_str().to_owned()),
            ),
            Self::Nickname(nick) => file::title_of(held.num, nick.as_str()),
        }
    }
}

/// Where undoing History's newest change takes `name` back to, read from History alone: the
/// number it replaced, where it moved the number, and the nickname it replaced, where it
/// changed the nickname. Who holds that number now is [`judge`]'s to say ([`undo_last`],
/// [`RenamePreview::undo`]): an undo is a checked rename back, and never restores a claim's class
/// or rank, since the membership log releases the number a free move leaves and a restored
/// automatic or seeded class would publish nothing to macOS ([`publish`]).
///
/// # Errors
/// History keeps no change, or only the first name, or a change that kept both number and
/// nickname ([`Cause::Nothing`]); its newest is an undo or was undone ([`Cause::Undone`]); the
/// name no longer stands as that change left it ([`Cause::Superseded`]); or the change gave this
/// Mac its first number ([`Cause::Unnumbered`]).
pub fn undo_to(name: &MachineName) -> Result<UndoTo, Refused> {
    let Some(change) = name.history.first() else {
        return Err(Refused::new(
            Cause::Nothing,
            "nothing to undo: no change to this Mac's name is kept".to_owned(),
        ));
    };
    match change.why {
        Why::First => {
            return Err(Refused::new(
                Cause::Nothing,
                "nothing to undo: the last change is this Mac's first name".to_owned(),
            ));
        }
        Why::Undone => {
            return Err(Refused::new(
                Cause::Undone,
                "the last change was an undo, which is not undone".to_owned(),
            ));
        }
        Why::Named | Why::Nickname | Why::Drawn | Why::Registry if change.undone => {
            return Err(Refused::new(
                Cause::Undone,
                "the last change was undone already".to_owned(),
            ));
        }
        Why::Named | Why::Nickname | Why::Drawn | Why::Registry => {}
    }
    if change.to != name.num || change.nick_to != name.shown_nick() {
        return Err(Refused::new(
            Cause::Superseded,
            format!(
                "this Mac's name changed after its last recorded change, which left it {}: \
                 there is no change of its to undo",
                file::title_of(change.to, change.nick_to.as_str())
            ),
        ));
    }
    let nick = change
        .nick_from
        .clone()
        .filter(|from| *from != change.nick_to);
    match (change.from, change.to) {
        (None, Some(_)) => Err(Refused::new(
            Cause::Unnumbered,
            "the last change gave this Mac its first number, and an undo leaves no Mac \
             unnumbered"
                .to_owned(),
        )),
        (Some(from), to) if Some(from) != to => Ok(UndoTo::Name(NameTo { num: from, nick })),
        _ => nick.map(UndoTo::Nickname).ok_or_else(|| {
            Refused::new(
                Cause::Nothing,
                "nothing to undo: the last change kept this Mac's number and nickname".to_owned(),
            )
        }),
    }
}

/// The number a change that keeps the claim publishes under ([`publish`]): the stored one while
/// it is chosen and no migration waits on it, else none, so a nickname on an automatic or seeded
/// number publishes nothing.
#[must_use]
pub fn chosen_number(name: &MachineName) -> Option<Number> {
    name.num
        .filter(|_| name.class == Class::Chosen && name.migration != Some(Migration::Waiting))
}

/// The number the undo of `name`'s newest change publishes under ([`publish`]), which its
/// caller reads the naming policy for before it undoes: the number it takes back, which the undo
/// claims as chosen, or for a nickname undo the chosen number it keeps ([`chosen_number`]);
/// `None` where History holds nothing to take back ([`undo_to`]) or the undo publishes nothing.
#[must_use]
pub fn undo_publishes(name: &MachineName) -> Option<Number> {
    match undo_to(name).ok()? {
        UndoTo::Name(to) => Some(to.num),
        UndoTo::Nickname(_) => chosen_number(name),
    }
}

/// Take back History's newest change for `kind` (**Undo**, single-level): a checked rename back
/// to the number it replaced ([`change_claim`], judged by [`judge`] as any rename is, so only a
/// number that is [`Verdict::Same`], [`Verdict::Free`] or [`Verdict::EarlierName`] is taken back,
/// never one another Mac now holds, by any claim, or the vault gives another Mac), with the
/// nickname it replaced, or a nickname change back alone ([`change_nick`], which leaves the claim
/// as it is). The change is marked undone and the undo recorded as [`Why::Undone`]. `seen` is the
/// newest change the caller read and showed, from a read that followed this Mac's registry row
/// ([`follow_registry`]) as the undo's own base does ([`edit_name`]): under the locks the undo
/// takes back only that one,
/// refusing ([`Cause::Unseen`]) where History's newest is another, so a name changed after the
/// caller read it is never taken somewhere the caller did not show; `None` (a caller that read no
/// name) takes back whatever History's newest is. `lab` is
/// read before any lock, and only for an undo that moves the number, as [`set_name`] reads it; a
/// number the lock then finds moved is refused rather than checked unread. A dry run makes no
/// node key and changes no existing name, though its first read makes the name as any first read
/// does, and its locks leave their lock files, as a rename's dry run does ([`set_name`]).
///
/// # Errors
/// As [`ensure_in_view`]; History's newest change is not `seen`; History holds nothing to take
/// back ([`undo_to`]); `lab` fails; the number or nickname is refused; a lock cannot be taken, or
/// the name cannot be written. Each refusal carries its [`Refused`] ([`Refused::of`]).
pub fn undo_last(
    state: &Path,
    seen: Option<&Change>,
    lab: impl FnOnce() -> io::Result<client::LabLabels>,
    kind: ByKind,
    roster: impl FnOnce() -> io::Result<Option<String>>,
    now_ms: u64,
    dry_run: bool,
) -> io::Result<NameSet> {
    let (_, origin) = ensure_in_view(state, roster, now_ms)?;
    let by = editor(state, kind, dry_run)?;
    let renumbers = file::read(state)?
        .as_ref()
        .and_then(|name| undo_to(name).ok())
        .is_some_and(|to| matches!(to, UndoTo::Name(_)));
    let lab = if renumbers {
        Some(read_lab(lab)?)
    } else {
        None
    };
    edit_name(state, by, origin, now_ms, dry_run, |held, locked| {
        if let Some(seen) = seen
            && held.history.first() != Some(seen)
        {
            return Err(Refused::new(
                Cause::Unseen,
                "this Mac's name changed after the undo read it, so nothing was undone: read the \
                 name again and undo over what it shows"
                    .to_owned(),
            )
            .into());
        }
        let to = undo_to(held)?;
        let mut marked = held.clone();
        if let Some(change) = marked.history.first_mut() {
            change.undone = true;
        }
        match to {
            UndoTo::Name(to) => {
                change_claim(&marked, &to, lab.as_ref(), locked, by, now_ms, Why::Undone)
            }
            UndoTo::Nickname(nick) => change_nick(&marked, nick, locked, by, now_ms, Why::Undone),
        }
    })
}

/// Retire this Mac's earlier number `num` (`retired`), or restore it, in `was` (§5.6 card 3), for
/// `kind`: a retired earlier number raises `top` only up to a person's cap
/// ([`aterm_types::machine_name::top`]). The NAME, nickname and History stay as they are; a
/// change raises `rev` and names this node, its key minted where it has none, as `by`. No
/// registry row changes.
///
/// # Errors
/// `num` is none of this Mac's earlier numbers ([`io::ErrorKind::NotFound`]); the node key cannot
/// be read or minted; the name lock cannot be taken, or the name read or replaced
/// ([`file::replace`]).
pub fn retire_was(state: &Path, num: Number, retired: bool, kind: ByKind) -> io::Result<NameSet> {
    let by = editor(state, kind, false)?;
    let mut changed = false;
    let name = file::replace(state, |stored| {
        let was = stored
            .was
            .iter_mut()
            .find(|was| was.num == num)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{num} is none of this Mac's earlier numbers"),
                )
            })?;
        if was.retired != retired {
            was.retired = retired;
            stored.rev = stored
                .rev
                .checked_add(1)
                .ok_or_else(|| io::Error::other("the name revision is exhausted"))?;
            stored.by = by;
            changed = true;
        }
        Ok(())
    })?;
    let me = by.map(|by| node_word(by.node));
    let shared = in_view(&taken_in(state, me.as_deref()), name.nick.as_str());
    Ok(NameSet {
        name,
        origin: Origin::Kept,
        changed,
        shared,
    })
}

/// Read what [`set_name`] judges a rename by, for [`RenamePreview::judge`]: the name as a first
/// read makes it ([`ensure_in_view`]), this node's books, and the vault's labels (`lab`), whose
/// failure the preview keeps as the answer for each number that moves this Mac's. It takes no
/// lock but the one a first read takes to make the name, and makes no node key.
///
/// # Errors
/// As [`ensure_in_view`]; the node identity or the books cannot be read.
pub fn rename_preview(
    state: &Path,
    lab: impl FnOnce() -> io::Result<client::LabLabels>,
    roster: impl FnOnce() -> io::Result<Option<String>>,
    now_ms: u64,
) -> io::Result<RenamePreview> {
    let (name, _) = ensure_in_view(state, roster, now_ms)?;
    let me = read_node_identity(state)?;
    let books = rename_books(state, now_ms)?;
    let own_node = me.is_some();
    let node = match &me {
        Some(me) => node_of(me.node())
            .ok_or_else(|| io::Error::other("this machine has no valid node id"))?,
        None => absent_node(&books.view)?,
    };
    let taken = books.taken_except(node);
    let mut view = books.view;
    let current = current_record(&name);
    let top = current.top;
    view.insert(node, Entry::Record(current));
    let settled = settle(&view);
    let lab = lab().map_err(|why| why.to_string());
    let kept = kept_labels(lab.as_ref().ok());
    Ok(RenamePreview {
        name,
        view,
        settled,
        top,
        node,
        own_node,
        lab,
        kept,
        taken,
    })
}

struct RenameBooks {
    view: BTreeMap<NodeId, Entry>,
    names: BTreeMap<NodeId, Nickname>,
}

impl RenameBooks {
    fn taken_except(&self, me: NodeId) -> Vec<Nickname> {
        self.names
            .iter()
            .filter(|(node, _)| **node != me)
            .map(|(_, nick)| nick.clone())
            .collect()
    }
}

/// The newest signed claim and display name for each node in both books. Failed reads stop
/// a rename; unsigned rows and stale records cannot reserve an ID or a display-name key.
fn rename_books(state: &Path, now_ms: u64) -> io::Result<RenameBooks> {
    use aterm_net::fleet_member::name_record::{Accepted, Source, accept};
    use aterm_net::fleet_member::{ISSUER_BOOK_FILE, PEERS_FILE, PEERS_FILE_MAX, Peer};
    let digest = fleet_seed::read(state)?.map_or(ZERO_DIGEST, |seed| seed.digest());
    let mark = match aterm_net::fleet::read_key_file(state) {
        Ok(secret) => Some(secret.mark()),
        Err(aterm_net::fleet::KeyFileError::Missing) => None,
        Err(why) => return Err(io::Error::other(why.to_string())),
    };
    let mut newest = BTreeMap::new();
    for file in [PEERS_FILE, ISSUER_BOOK_FILE] {
        let bytes = match read_private(state, file, PEERS_FILE_MAX) {
            Ok(bytes) => bytes,
            Err(PrivateFileError::Missing) => continue,
            Err(why) => return Err(io::Error::other(format!("{file}: {why}"))),
        };
        let text = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
        for line in text.lines().map(str::trim).filter(|line| {
            !line.is_empty() && !line.starts_with('#') && !line.starts_with("issuer node=")
        }) {
            let peer = Peer::parse(line)
                .ok_or_else(|| io::Error::other(format!("{file} holds a malformed peer")))?;
            if peer.fleet.is_some_and(|fleet| Some(fleet) != mark) {
                continue;
            }
            if !peer.signature_holds() {
                continue;
            }
            let (Some(word), Some(key), Some(signed)) = (&peer.node, peer.key, &peer.signed) else {
                continue;
            };
            let Some(node) = node_of(word) else {
                continue;
            };
            let primary = peer
                .primary_name()
                .and_then(|name| Number::parse(name).ok());
            let accepted = signed.record().and_then(|carried| {
                accept(carried, (word, &key), digest, Source::Entry, None, now_ms).ok()
            });
            let verified = accepted.is_some();
            let (entry, nick, eseq, rev) = match accepted {
                Some(Accepted::Record { record, .. }) => {
                    let (eseq, rev) = (record.eseq, record.rev);
                    (Entry::Record(record.claim), record.nick, eseq, rev)
                }
                Some(Accepted::Ahead(record)) => (
                    Entry::Unknown(record.claim.num.or(primary)),
                    None,
                    record.eseq,
                    record.rev,
                ),
                None if signed.record().is_some() => (Entry::Unknown(primary), None, 0, 0),
                None => (
                    primary.map_or(Entry::Unknown(None), Entry::Implied),
                    None,
                    0,
                    0,
                ),
            };
            // The record has its own signed currency. A newer descriptor can carry a stale
            // record or have had its record stripped by an older relay; neither rolls back a
            // record already verified in this view.
            let rank = (u8::from(verified), eseq, rev, signed.seq);
            if newest.get(&node).is_none_or(|(held, _, _)| rank > *held) {
                newest.insert(node, (rank, entry, nick));
            }
        }
    }
    let mut view = BTreeMap::new();
    let mut names = BTreeMap::new();
    for (node, (_, entry, nick)) in newest {
        view.insert(node, entry);
        if let Some(nick) = nick {
            names.insert(node, nick);
        }
    }
    Ok(RenameBooks { view, names })
}

/// `name` with the nickname `nick`, set by `by`: `chosen`, and the claim (NAME, class, `since`,
/// `act`) as it was, so a nickname change never weakens the NAME's precedence (§5.4); `rev`
/// rises for every changed nickname. `None` when `name` holds `nick` as a
/// chosen nickname already.
#[must_use]
pub fn renicked(name: &MachineName, nick: Nickname, by: Option<By>) -> Option<MachineName> {
    if name.nick == nick && name.nick_class == NickClass::Chosen {
        return None;
    }
    let mut next = name.clone();
    next.nick = nick;
    next.nick_class = NickClass::Chosen;
    next.rev = name.rev.saturating_add(1);
    next.by = by;
    Some(next)
}

/// Set this machine's nickname to `to` for `kind`, after making the name where none is there
/// ([`ensure_in_view`], which reads `roster` only then). Up to eight random draws precede a
/// bounded pool search for a noun neither in view nor held; either nickname is `chosen`,
/// since a person's press or an agent's act chose it, and so never changes by itself. `by` names
/// this node, minted where it has none. A dry run changes no nickname and mints no node, though
/// its read makes the name as any first read does.
/// The book and name locks serialize edits with incoming records. A chosen name is refused when another
/// node's signed record carries its canonical key; no book record replaces the local number.
/// A change is recorded in History: [`Why::Nickname`] for a word, [`Why::Drawn`] for a draw.
///
/// # Errors
/// As [`ensure_in_view`]; the node key cannot be read or minted; fleet state or books cannot be
/// read; the CSPRNG cannot be read; a book or name lock cannot be taken; another node holds the
/// chosen name; the file cannot be replaced
/// ([`file::replace`]).
pub fn set_nick(
    state: &Path,
    to: &NickTo,
    kind: ByKind,
    roster: impl FnOnce() -> io::Result<Option<String>>,
    now_ms: u64,
    dry_run: bool,
) -> io::Result<NickSet> {
    let (_, origin) = ensure_in_view(state, roster, now_ms)?;
    let by = editor(state, kind, dry_run)?;
    let why = match to {
        NickTo::Word(_) => Why::Nickname,
        NickTo::Random => Why::Drawn,
    };
    let set = edit_name(state, by, origin, now_ms, dry_run, |stored, locked| {
        let nick = match to {
            NickTo::Word(word) => word.clone(),
            NickTo::Random => file::draw_nick(|noun| {
                in_view(&locked.taken, noun) || noun.eq_ignore_ascii_case(stored.nick.as_str())
            })?,
        };
        change_nick(stored, nick, locked, by, now_ms, why)
    })?;
    Ok(NickSet {
        name: set.name,
        origin: set.origin,
        changed: set.changed,
        shared: set.shared,
    })
}

/// `held` with the chosen nickname `nick` ([`renicked`]), refused where another node's record
/// carries it; a change records one [`Change`] for `why`, and the claim stays as it was.
///
/// # Errors
/// Another node's record carries `nick` ([`Cause::Nickname`]).
fn change_nick(
    held: &MachineName,
    nick: Nickname,
    locked: &Locked,
    by: Option<By>,
    now_ms: u64,
    why: Why,
) -> io::Result<MachineName> {
    require_free_nick(&locked.taken, &nick)?;
    Ok(renicked(held, nick, by).map_or_else(
        || held.clone(),
        |mut next| {
            let change = Change::between(held, &next, now_ms, why);
            next.record(change);
            next
        },
    ))
}

/// The vault whose standing says whether this Mac is in a fleet ([`InFleet::read`]), found where
/// `$HOME` names it and never made or enrolled (a reader leaves the vault folder as it finds it):
/// `None` where no home directory is known or this process runs under a private state root,
/// whose vault is not its own (as [`vault_roster_label`] reads none there).
#[must_use]
pub fn standing_vault() -> Option<client::Paths> {
    if aterm_types::dirs::runs_under_private_state_root() {
        return None;
    }
    client::Paths::locate().ok()
}

/// The numbers this Mac's vault gives other Macs ([`client::lab_labels`]), which
/// [`set_name`] holds a rename off: nothing where no vault is read ([`standing_vault`]).
///
/// # Errors
/// The vault is set up and its labels cannot be read, which a rename must not take as free.
pub fn lab_labels(vault: Option<&client::Paths>) -> io::Result<client::LabLabels> {
    vault.map_or_else(
        || Ok(client::LabLabels::default()),
        |paths| {
            client::lab_labels(paths)
                .map_err(|e| io::Error::other(format!("the vault's labels are unread: {e}")))
        },
    )
}

/// This machine's label in the vault's swarm roster, which [`ensure`] reads only during
/// first-name fallback after an unbound registry: `None` where no vault is set up here, its roster names this machine
/// nowhere, or this process runs under a private state root, whose vault is not its own.
///
/// # Errors
/// The vault's state cannot be read, or a writer held it past the read's wait.
pub fn vault_roster_label() -> io::Result<Option<String>> {
    // A private state root (a test binary, an instance on scratch state) is no machine's own
    // account: the vault `$HOME` names is the person's, whose NAME is not the scratch state's.
    if aterm_types::dirs::runs_under_private_state_root() {
        return Ok(None);
    }
    let paths = client::Paths::live().map_err(|e| io::Error::other(format!("the vault: {e}")))?;
    client::resolve_local_swarm_label(&paths, &[])
        .map_err(|e| io::Error::other(format!("the vault's roster: {e}")))
}

/// The nicknames other nodes' records in this node's books carry (§2.4), each verified under
/// its entry's key and the seed digest held in `state`, else the zero digest
/// ([`nicknames_in_books`]); `me` is this node, whose own records it passes over.
#[must_use]
pub fn taken_in(state: &Path, me: Option<&str>) -> Vec<Nickname> {
    let digest = fleet_seed::read(state)
        .ok()
        .flatten()
        .map_or(ZERO_DIGEST, |seed| seed.digest());
    nicknames_in_books(state, me, digest)
}

/// Whether `word` is one of the nicknames `taken`, in any case (§2.4).
#[must_use]
pub fn in_view(taken: &[Nickname], word: &str) -> bool {
    let key = Nickname::canonical(word);
    taken
        .iter()
        .any(|nick| Nickname::canonical(nick.as_str()) == key)
}

fn require_free_nick(taken: &[Nickname], nick: &Nickname) -> io::Result<()> {
    if in_view(taken, nick.as_str()) {
        return Err(nick_used(nick).into());
    }
    Ok(())
}

/// The refusal of a nickname another node's record carries.
fn nick_used(nick: &Nickname) -> Refused {
    Refused::new(
        Cause::Nickname,
        format!(
            "The name {} is already used by another machine.",
            nick.as_str()
        ),
    )
}

/// What [`ensure`] writes when no file is there.
fn first(
    state: &Path,
    roster: impl FnOnce() -> io::Result<Option<String>>,
    in_use: impl Fn(&str) -> bool,
    now_ms: u64,
) -> io::Result<(MachineName, Origin)> {
    let legacy = legacy_name(state)?;
    // Keep identity admission failures distinct from healthy absence during legacy fallback.
    // The identity supplies no name or history: the registry and local file are authoritative.
    let _ = read_node_identity(state)?;
    let roster = roster()?;
    let num = roster
        .as_deref()
        .and_then(bare)
        .or_else(|| legacy.as_deref().and_then(bare));
    let word = legacy
        .as_deref()
        .filter(|word| bare(word).is_none())
        .and_then(|word| Nickname::parse(word).ok());
    let mut origin = Origin::Drawn;
    let mut name = match word {
        Some(word) => {
            origin = Origin::Evidence;
            let mut name = MachineName::unnumbered(word, now_ms);
            name.nick_class = NickClass::Chosen;
            name.migration = Some(Migration::Pending);
            name
        }
        None => MachineName::unnumbered(file::draw_nick(in_use)?, now_ms),
    };
    if let Some(num) = num {
        origin = Origin::Evidence;
        name.num = Some(num);
        name.class = Class::Seeded;
        name.since = IMPLIED_SINCE;
        name.migration = Some(Migration::Pending);
    }
    Ok((name, origin))
}

/// What an older build's `fleet-name` holds: `None` where there is none or its text is no name
/// (too long, not UTF-8, outside the fleet-name grammar), which no later read changes.
///
/// # Errors
/// It is there and cannot be read: not private, a symlink, or an I/O error.
fn legacy_name(state: &Path) -> io::Result<Option<String>> {
    match read_private(state, NAME_FILE, NAME_FILE_MAX) {
        Ok(_) | Err(PrivateFileError::Missing | PrivateFileError::Malformed(_)) => {
            Ok(read_primary_name(state).ok().flatten())
        }
        Err(e) => Err(io::Error::other(format!("{NAME_FILE}: {e}"))),
    }
}

/// `word` when it is a NAME alone, as an older build stored one (`m8`, never `m8-studio` or
/// `8`).
fn bare(word: &str) -> Option<Number> {
    Number::leading(word).filter(|num| num.to_string() == word)
}

#[cfg(test)]
pub(crate) mod tests;
