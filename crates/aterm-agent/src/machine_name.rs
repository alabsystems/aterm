// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This machine's name (`docs/DESIGN-machine-names-2026-11-04.md`): its NAME (`m8`, or `m0`
//! while unnumbered), nickname and earlier numbers, kept in `machine-name.toml` whether or not it
//! is in a fleet ([`file`]), and made at first run by [`ensure`].

pub mod file;

use std::cell::OnceCell;
use std::io;
use std::path::Path;

use aterm_net::fleet_member::fleet_seed;
use aterm_net::fleet_member::name_record::{
    By, ByKind, NickClass, ZERO_DIGEST, nicknames_in_books, node_of, own_newest,
};
use aterm_net::fleet_member::{
    NAME_FILE, NAME_FILE_MAX, PrivateFileError, node_identity, read_node_identity,
    read_primary_name, read_private,
};
use aterm_types::machine_name::{Class, IMPLIED_SINCE, Nickname, Number};

use file::{MachineName, Migration};

/// Where the name [`ensure`] answers came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// `machine-name.toml` was there, or another first reader wrote it first.
    Kept,
    /// This node's own newest verified record in its books (Floor, §3.2).
    Floor,
    /// Evidence from before records, pending migration (§7.1), which judges its time: the NAME
    /// an older build's `fleet-name` holds, else the roster label's, and a word-valued
    /// `fleet-name` as the nickname.
    Evidence,
    /// None of these: `m0` and a drawn noun (§2.4).
    Drawn,
}

/// This machine's name, made by the first reader to find no `machine-name.toml` in `state`
/// (§3.3), which writes it under its lock ([`file::read_or_create`]), so every other reader
/// reads the same one back.
///
/// It is the first of these that stands: this node's own newest record that verifies under its
/// key (Floor, §3.2, under the seed it holds, the digest kept from its last leave or the zero
/// digest, [`fleet_seed::floor_digests`]), so a deleted or lost file never shows `m0` on a
/// numbered machine; else evidence from before
/// records as a `seeded` claim at the implied rank, pending migration (§7.1, which judges its
/// time), so an established Mac never shows `m0` either: the bare NAME an older build's
/// `fleet-name` holds, else the bare NAME `roster` gives
/// (`aterm_secrets::client::roster::resolve_local_swarm_label`), read only when neither of
/// those names the claim, with a word-valued `fleet-name` as its chosen nickname; else `m0` and
/// a noun `in_use` does not name, drawn. A record that carries no nickname keeps the evidence's,
/// else the drawn one. A `fleet-name` whose text is no name is no evidence; one that cannot be
/// read, a node key that cannot be read and a roster that cannot be read stop creation, so no
/// passing failure is kept as `m0` and the next reader tries again (§11.79).
///
/// # Errors
/// `machine-name.toml` cannot be read or written, `fleet-name`, the node key or `roster`
/// cannot be read, or the CSPRNG cannot be read.
pub fn ensure(
    state: &Path,
    roster: impl FnOnce() -> io::Result<Option<String>>,
    in_use: impl Fn(&str) -> bool,
    now_ms: u64,
) -> io::Result<(MachineName, Origin)> {
    let mut origin = Origin::Kept;
    let (name, created) = file::read_or_create(state, || {
        let (name, first) = first(state, roster, in_use, now_ms)?;
        origin = first;
        Ok(name)
    })?;
    Ok((name, if created { origin } else { Origin::Kept }))
}

/// [`ensure`] as a reader on this machine runs it, which `aterm name` and the menu bar share:
/// `roster` read only where nothing else names the claim, and a drawn noun redrawn while
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
    /// Where the name it changed came from: made by this read, or kept.
    pub origin: Origin,
    /// The nickname changed: it was another word, or this word drawn rather than chosen.
    pub changed: bool,
    /// Another node's record in this node's books carries the nickname too: information only,
    /// since two machines may share one (§5.4 `Shared`).
    pub shared: bool,
}

/// `name` with the nickname `nick`, set by `by`: `chosen`, and the claim (NAME, class, `since`,
/// `act`) as it was, so a nickname change never weakens the NAME's precedence (§5.4); `rev`
/// rises once a record has seeded the file, and a file no record seeded stays at `rev` 0, below
/// every record this node can have signed, so a later Floor still adopts this node's own claim
/// (§11.73) rather than a nickname change outranking it. `None` when `name` holds `nick` as a
/// chosen nickname already.
#[must_use]
pub fn renicked(name: &MachineName, nick: Nickname, by: Option<By>) -> Option<MachineName> {
    if name.nick == nick && name.nick_class == NickClass::Chosen {
        return None;
    }
    let mut next = name.clone();
    next.nick = nick;
    next.nick_class = NickClass::Chosen;
    if name.rev > 0 {
        next.rev = name.rev.saturating_add(1);
    }
    next.by = by;
    Some(next)
}

/// Set this machine's nickname to `to` for `kind`, after making the name where none is there
/// ([`ensure_in_view`], which reads `roster` only then). A drawn noun is redrawn, up to eight
/// times, while it is one of the nicknames in view or the one held; either nickname is `chosen`,
/// since a person's press or an agent's act chose it, and so never changes by itself. `by` names
/// this node, minted where it has none. A dry run changes no nickname and mints no node, though
/// its read makes the name as any first read does.
///
/// # Errors
/// As [`ensure_in_view`]; the node key cannot be read or minted; the CSPRNG cannot be read; the
/// file cannot be replaced ([`file::replace`]).
pub fn set_nick(
    state: &Path,
    to: &NickTo,
    kind: ByKind,
    roster: impl FnOnce() -> io::Result<Option<String>>,
    now_ms: u64,
    dry_run: bool,
) -> io::Result<NickSet> {
    let (held, origin) = ensure_in_view(state, roster, now_ms)?;
    let me = if dry_run {
        read_node_identity(state)?
    } else {
        Some(node_identity(state)?)
    };
    let me = me.as_ref().map(|me| me.node().to_owned());
    let taken = taken_in(state, me.as_deref());
    let nick = match to {
        NickTo::Word(word) => word.clone(),
        NickTo::Random => file::draw_nick(|noun| {
            in_view(&taken, noun) || noun.eq_ignore_ascii_case(held.nick.as_str())
        })?,
    };
    let shared = in_view(&taken, nick.as_str());
    let by = me
        .as_deref()
        .map(|node| {
            node_of(node)
                .map(|node| By { node, kind })
                .ok_or_else(|| io::Error::other(format!("node `{node}` is no node id")))
        })
        .transpose()?;
    if dry_run {
        let next = renicked(&held, nick, by);
        let changed = next.is_some();
        return Ok(NickSet {
            name: next.unwrap_or(held),
            origin,
            changed,
            shared,
        });
    }
    let mut changed = false;
    let name = file::replace(state, |name| {
        if let Some(next) = renicked(name, nick, by) {
            *name = next;
            changed = true;
        }
        Ok(())
    })?;
    Ok(NickSet {
        name,
        origin,
        changed,
        shared,
    })
}

/// This machine's label in the vault's swarm roster, which [`ensure`] reads only where nothing
/// else names the claim: `None` where no vault is set up here, its roster names this machine
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
    let paths = aterm_secrets::client::Paths::live()
        .map_err(|e| io::Error::other(format!("the vault: {e}")))?;
    aterm_secrets::client::resolve_local_swarm_label(&paths, &[])
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
    let floor = read_node_identity(state)?
        .and_then(|me| own_newest(state, &me, &fleet_seed::floor_digests(state)));
    let num = match legacy.as_deref().and_then(bare) {
        Some(num) => Some(num),
        // Floor's claim replaces the evidence's whole, so its roster read would be wasted.
        None if floor.is_none() => roster()?.as_deref().and_then(bare),
        None => None,
    };
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
    if let Some((record, digest)) = floor {
        origin = Origin::Floor;
        name.adopt(&record, digest);
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
