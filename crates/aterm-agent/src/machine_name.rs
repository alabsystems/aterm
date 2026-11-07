// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This machine's name (`docs/DESIGN-machine-names-2026-11-04.md`): its NAME (`m8`, or `m0`
//! while unnumbered), nickname and earlier numbers, kept in `machine-name.toml` whether or not it
//! is in a fleet ([`file`]), made at first run by [`ensure`], and, once a number is chosen,
//! published to its other readers ([`publish`]).

pub mod file;
pub mod publish;

use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use aterm_net::fleet_member::fleet_seed;
use aterm_net::fleet_member::name_record::{
    By, ByKind, NickClass, ZERO_DIGEST, nicknames_in_books, node_of,
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

use file::{MachineName, Migration};

use crate::members_cli::Standing;

/// Whether this Mac is in a fleet, as the machine title's row and `aterm name`'s `fleet=` say it
/// (`docs/DESIGN-machine-names-2026-11-04.md` §11.84, §11.85): its vault's membership standing
/// ([`Standing`]), with a shared-secret fleet's key standing for membership until the one-mesh
/// design's D2 deletes the key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InFleet {
    /// A member, or a holder of a fleet key that reads.
    In,
    /// On its way in: let in and not yet recorded, asking, or let in with no seat yet.
    Joining,
    /// Never in, taken out, or let go.
    Out,
    /// The vault's local state could not be read: why.
    Unread(String),
}

impl InFleet {
    /// What `standing` says, where `fleet_key` is whether a fleet key reads and `revoked` whether
    /// the vault let this Mac go or took it out ([`client::Membership::Revoked`]), which outranks
    /// a fleet key: the key it still holds is a secret it was let go from, not a membership.
    #[must_use]
    pub fn of(fleet_key: bool, revoked: bool, standing: &Standing) -> Self {
        if revoked || matches!(standing, Standing::Dropped(_)) {
            return Self::Out;
        }
        if fleet_key {
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
    /// a keyless, nonrevoked Mac needs its standing read too.
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
            return Self::In;
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
            Self::Unread(_) => "unread",
        }
    }

    /// The machine menu's words: `in a fleet`, `joining a fleet`, `not in a fleet`, or
    /// `fleet not read`.
    #[must_use]
    pub const fn phrase(&self) -> &'static str {
        match self {
            Self::In => "in a fleet",
            Self::Joining => "joining a fleet",
            Self::Out => "not in a fleet",
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
    /// None of these: `m0` and a drawn noun (§2.4).
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
/// if no other reader created the file first. Every reader returns the winner.
///
/// A valid bound registry machine row supplies the name first, as a seeded local mirror with
/// no pending book migration. Its nickname retains its provenance, or a null nickname draws a
/// local automatic noun. A pending registry projection or invalid bound name stops creation.
/// Without a binding, the roster's bare NAME wins, then an older `fleet-name`'s bare NAME,
/// else `m0` and a noun `in_use` does not name, drawn. A word-valued `fleet-name` supplies its
/// chosen nickname. Legacy numbering is seeded at the implied rank with migration pending.
/// Signed book records never restore or replace this local mirror. A `fleet-name` whose text
/// is no name is no evidence; one that cannot be
/// read, a node key that cannot be read and a roster that cannot be read stop creation, so no
/// passing failure is kept as `m0` and the next reader tries again (§11.79).
///
/// # Errors
/// `machine-name.toml` cannot be read or written; or, while no other reader supplies the name,
/// the registry name, `fleet-name`, the node key, `roster` or the CSPRNG cannot be read.
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
        let (candidate, source) = prepared?;
        origin = source;
        Ok(candidate)
    })?;
    Ok((name, if created { origin } else { Origin::Kept }))
}

fn first_registry(
    candidate: RegistryName,
    in_use: impl Fn(&str) -> bool,
    now_ms: u64,
) -> io::Result<(MachineName, Origin)> {
    let (nick, nick_class) = match candidate.nick {
        Some(nick) => nick,
        None => (file::draw_nick(in_use)?, NickClass::Auto),
    };
    let mut name = MachineName::unnumbered(nick, now_ms);
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
    /// Another node's record in this node's books carries the nickname too: information only,
    /// since two machines may share one (§5.4 `Shared`).
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
    /// Another node in view carries the nickname too.
    pub shared: bool,
}

/// Choose this machine's number and optionally its nickname, atomically under the name lock.
/// The book lock is held first so a signed claim cannot arrive between the conflict check and
/// the write. The local file is the edit's sole base; its own older signed records cannot
/// replace it. A number held by another signed node is refused until the registry's conflict
/// resolution can move that node. So is a
/// number the vault holds for another Mac or role (`lab`, read before any lock is taken:
/// [`lab_labels`], its roster, log seats and policy, which other Macs reach a Mac by on today's
/// lab, §11.86 of `docs/DESIGN-machine-names-2026-11-04.md`), and a move off the number the
/// policy names this Mac's vault server by, which other Macs dial as `m<N>.local` and which the
/// macOS names follow while identity naming is on. `lab` is not read for a request that keeps the
/// stored number, a nickname change typed with it; should a concurrent local edit change that
/// number before the name lock is taken, the request is refused rather than checked unread.
/// The earlier local number and claim are kept for aliases and undo. A dry run makes no node key and
/// changes no existing name, though its first read makes the name as any first read does.
///
/// # Errors
/// As [`ensure_in_view`]; `lab` fails; the node, seed or books cannot be read, a signed node or
/// the vault holds the number, the move would leave the policy's server name, the stored number
/// moved under the lock while `lab` was unread, a lock cannot be taken, or the changed name
/// cannot be written.
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
    let me = if dry_run {
        read_node_identity(state)?
    } else {
        Some(node_identity(state)?)
    };
    let by = me
        .as_ref()
        .map(|me| {
            node_of(me.node())
                .map(|node| By { node, kind })
                .ok_or_else(|| io::Error::other("this machine has no valid node id"))
        })
        .transpose()?;
    // Whether the request keeps the stored number, as the lock below reads it again.
    let keeps = file::read(state)?.is_some_and(|name| !moves(&name, to.num));
    let lab = if keeps { None } else { Some(lab()?) };
    let kept = kept_labels(lab.as_ref());
    aterm_net::fleet_member::write_held(state, aterm_net::fleet_member::Began::Here, |_| {
        let mut view = rename_view(state, now_ms)?;
        let node = match by {
            Some(by) => by.node,
            None => absent_node(&view)?,
        };
        let mut changed = false;
        let mut preview = None;
        let name = file::replace(state, |stored| {
            let held = &*stored;
            let current = current_record(held);
            view.insert(node, Entry::Record(current.clone()));
            let settled = settle(&view);
            let fleet = Fleet {
                view: &view,
                settled: &settled,
                top: current.top,
                kept: &kept,
                labels: &BTreeMap::new(),
            };
            judge(to.num, held, lab.as_ref(), &fleet, node).map_err(io::Error::other)?;
            let mut next = held.clone();
            let renumber = current.num != Some(to.num) || current.class != Class::Chosen;
            if renumber {
                let latest = view
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
                    Some((current.since, current.base_or(node), generation))
                } else {
                    None
                };
                let claim = renamed(&current, to.num, take, stamp);
                next.undo = Some(file::Held {
                    num: held.num,
                    class: held.class,
                    since: held.since,
                    base: held.base,
                    tgen: held.tgen,
                    sgen: held.sgen,
                });
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
                next.nick = nick.clone();
                next.nick_class = NickClass::Chosen;
            }
            if next != *held {
                next.rev = held
                    .rev
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("the name revision is exhausted"))?;
                next.by = by;
            }
            changed = next != *stored;
            if dry_run {
                preview = Some(next);
            } else {
                *stored = next;
            }
            Ok(())
        })?;
        let name = preview.unwrap_or(name);
        let shared = in_view(
            &taken_in(state, me.as_ref().map(|me| me.node())),
            name.nick.as_str(),
        );
        Ok(NameSet {
            name,
            origin,
            changed,
            shared,
        })
    })
    .map_err(|e| io::Error::other(e.to_string()))?
}

/// Whether selecting `num` makes a new explicit claim: a different number, an automatic or
/// seeded number not chosen yet, or a number a migration still waits on.
fn moves(held: &MachineName, num: Number) -> bool {
    held.num != Some(num)
        || held.class != Class::Chosen
        || held.migration == Some(Migration::Waiting)
}

/// The vault's labels as the pure check keeps them: each number held for another Mac or role,
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
/// refuse a number in the same words.
///
/// # Errors
/// Why the rename is refused: the number moved while `lab` was unread, the move leaves the
/// policy's server name, or the check gives the number to another node, Mac or role.
fn judge(
    num: Number,
    held: &MachineName,
    lab: Option<&client::LabLabels>,
    fleet: &Fleet<'_>,
    node: NodeId,
) -> Result<Verdict, String> {
    if moves(held, num) && lab.is_none() {
        return Err(format!(
            "this Mac's number moved while the rename was read, so {num} was not checked \
             against the vault: rename again"
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
        return Err(format!(
            "the vault's policy names this Mac's server {dialed}, which other Macs dial as \
             {dialed}.local: name it {num} in the policy's servers first"
        ));
    }
    let verdict = match check(&num.to_string(), fleet, node) {
        // Same is a no-op for a chosen number, but selecting a seeded/automatic number
        // makes a new claim. Its current lab reservation still governs that choice.
        Verdict::Same if moves(held, num) && lab.is_some_and(|lab| lab.held.contains_key(&num)) => {
            Verdict::Labelled { by: None }
        }
        verdict => verdict,
    };
    match verdict {
        Verdict::Same | Verdict::Free | Verdict::EarlierName { .. } => Ok(verdict),
        Verdict::TakesAuto { from } => Err(format!("{num} already belongs to {from}")),
        Verdict::HeldByPerson { by }
        | Verdict::HeldSeeded { by }
        | Verdict::HeldByOlderBuild { by } => Err(format!("{num} already belongs to {by}")),
        Verdict::SecondNode { of } | Verdict::OtherAccountNamed { node: of } => Err(format!(
            "another account on this machine ({of}) holds its number"
        )),
        Verdict::Unconfirmed { .. } => {
            Err(format!("{num} is waiting for its holder's name record"))
        }
        Verdict::Labelled { .. } => Err(lab.and_then(|lab| lab.held.get(&num)).map_or_else(
            || format!("{num} is another Mac's label in this fleet"),
            |hold| format!("{num} is {}", hold.phrase()),
        )),
        _ => Err(format!("{num} is not available in this fleet")),
    }
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

    /// What [`set_name`] would answer a rename to `num` from this read: the verdict it renames by
    /// ([`Verdict::Same`], [`Verdict::Free`] or [`Verdict::EarlierName`]).
    ///
    /// # Errors
    /// The words [`set_name`] refuses `num` with, and, for a number that moves this Mac's, why
    /// the vault's labels were not read.
    pub fn judge(&self, num: Number) -> Result<Verdict, String> {
        // As `set_name`: the vault's labels are read, and judged, only for a number that moves.
        let lab = if moves(&self.name, num) {
            Some(self.lab.as_ref().map_err(String::clone)?)
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

    /// Whether another node's record in this node's books carries `nick` too: information only,
    /// since two machines may share one (§5.4 `Shared`).
    #[must_use]
    pub fn shared(&self, nick: &Nickname) -> bool {
        in_view(&self.taken, nick.as_str())
    }

    /// Why the vault's labels were not read, when `num` moves this Mac's number and so needs them:
    /// [`RenamePreview::judge`] then refuses `num` in these words, while [`set_name`] reads the
    /// labels again at the set and may find them.
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

    /// This read as though other nodes' records carried `taken`, for another crate's test of what
    /// it says of a shared nickname.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_taken_for_test(mut self, taken: Vec<Nickname>) -> Self {
        self.taken = taken;
        self
    }
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
    let mut view = rename_view(state, now_ms)?;
    let node = match &me {
        Some(me) => node_of(me.node())
            .ok_or_else(|| io::Error::other("this machine has no valid node id"))?,
        None => absent_node(&view)?,
    };
    let current = current_record(&name);
    let top = current.top;
    view.insert(node, Entry::Record(current));
    let settled = settle(&view);
    let lab = lab().map_err(|why| why.to_string());
    let kept = kept_labels(lab.as_ref().ok());
    let taken = taken_in(state, me.as_ref().map(|me| me.node()));
    Ok(RenamePreview {
        name,
        view,
        settled,
        top,
        node,
        lab,
        kept,
        taken,
    })
}

/// The newest signed claim for each node in both books. Failed reads stop a rename rather
/// than treating an unread book as empty; unsigned rows cannot reserve a machine number.
fn rename_view(state: &Path, now_ms: u64) -> io::Result<BTreeMap<NodeId, Entry>> {
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
            let (entry, eseq, rev) = match accepted {
                Some(Accepted::Record { record, .. }) => {
                    let (eseq, rev) = (record.eseq, record.rev);
                    (Entry::Record(record.claim), eseq, rev)
                }
                Some(Accepted::Ahead(record)) => (
                    Entry::Unknown(record.claim.num.or(primary)),
                    record.eseq,
                    record.rev,
                ),
                None if signed.record().is_some() => (Entry::Unknown(primary), 0, 0),
                None => (primary.map_or(Entry::Unknown(None), Entry::Implied), 0, 0),
            };
            // The record has its own signed currency. A newer descriptor can carry a stale
            // record or have had its record stripped by an older relay; neither rolls back a
            // record already verified in this view.
            let rank = (u8::from(verified), eseq, rev, signed.seq);
            if newest.get(&node).is_none_or(|(held, _)| rank > *held) {
                newest.insert(node, (rank, entry));
            }
        }
    }
    Ok(newest
        .into_iter()
        .map(|(node, (_, entry))| (node, entry))
        .collect())
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
/// ([`ensure_in_view`], which reads `roster` only then). A drawn noun is redrawn, up to eight
/// times, while it is one of the nicknames in view or the one held; either nickname is `chosen`,
/// since a person's press or an agent's act chose it, and so never changes by itself. `by` names
/// this node, minted where it has none. A dry run changes no nickname and mints no node, though
/// its read makes the name as any first read does.
/// The local name lock serializes edits; nickname collision checks are advisory, and no book
/// record can replace the local name or its number during a nickname edit.
///
/// # Errors
/// As [`ensure_in_view`]; the node key cannot be read or minted; the CSPRNG cannot be read; the
/// name lock cannot be taken; the file cannot be replaced ([`file::replace`]).
pub fn set_nick(
    state: &Path,
    to: &NickTo,
    kind: ByKind,
    roster: impl FnOnce() -> io::Result<Option<String>>,
    now_ms: u64,
    dry_run: bool,
) -> io::Result<NickSet> {
    let (_, origin) = ensure_in_view(state, roster, now_ms)?;
    let me = if dry_run {
        read_node_identity(state)?
    } else {
        Some(node_identity(state)?)
    };
    let by = me
        .as_ref()
        .map(|me| {
            node_of(me.node())
                .map(|node| By { node, kind })
                .ok_or_else(|| io::Error::other("this machine has no valid node id"))
        })
        .transpose()?;
    let taken = taken_in(state, me.as_ref().map(|me| me.node()));
    let mut changed = false;
    let mut preview = None;
    let name = file::replace(state, |stored| {
        let nick = match to {
            NickTo::Word(word) => word.clone(),
            NickTo::Random => file::draw_nick(|noun| {
                in_view(&taken, noun) || noun.eq_ignore_ascii_case(stored.nick.as_str())
            })?,
        };
        let next = renicked(stored, nick, by).unwrap_or_else(|| stored.clone());
        changed = next != *stored;
        if dry_run {
            preview = Some(next);
        } else {
            *stored = next;
        }
        Ok(())
    })?;
    let name = preview.unwrap_or(name);
    let shared = in_view(&taken, name.nick.as_str());
    Ok(NickSet {
        name,
        origin,
        changed,
        shared,
    })
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

/// The numbers this Mac's vault gives other Macs and roles ([`client::lab_labels`]), which
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
    taken
        .iter()
        .any(|nick| nick.as_str().eq_ignore_ascii_case(word))
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
