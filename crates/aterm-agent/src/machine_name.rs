// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This machine's name (`docs/DESIGN-machine-names-2026-11-04.md`): its NAME (`m8`, or `m0`
//! while unnumbered), nickname and earlier numbers, kept in `machine-name.toml` whether or not it
//! is in a fleet ([`file`]), and made at first run by [`ensure`].

pub mod file;

use std::io;
use std::path::Path;

use aterm_net::fleet_member::fleet_seed;
use aterm_net::fleet_member::name_record::{NickClass, own_newest};
use aterm_net::fleet_member::{read_node_identity, read_primary_name};
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
/// (`aterm_secrets::client::roster::resolve_local_swarm_label`), with a word-valued
/// `fleet-name` as its chosen nickname; else `m0` and a noun `in_use` does not name, drawn. A
/// record that carries no nickname keeps the evidence's, else the drawn one. A node key or
/// `fleet-name` that cannot be read is no evidence: the file made without it is at `rev` 0,
/// below every record of this node's, so the Floor of the next `reconcile` heals it.
///
/// # Errors
/// `machine-name.toml` cannot be read or written, or the CSPRNG cannot be read.
pub fn ensure(
    state: &Path,
    roster: Option<&str>,
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

/// What [`ensure`] writes when no file is there.
fn first(
    state: &Path,
    roster: Option<&str>,
    in_use: impl Fn(&str) -> bool,
    now_ms: u64,
) -> io::Result<(MachineName, Origin)> {
    let legacy = read_primary_name(state).ok().flatten();
    let num = legacy
        .as_deref()
        .and_then(bare)
        .or_else(|| roster.and_then(bare));
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
    if let Ok(Some(me)) = read_node_identity(state)
        && let Some((record, digest)) = own_newest(state, &me, &fleet_seed::floor_digests(state))
    {
        origin = Origin::Floor;
        name.adopt(&record, digest);
    }
    Ok((name, origin))
}

/// `word` when it is a NAME alone, as an older build stored one (`m8`, never `m8-studio` or
/// `8`).
fn bare(word: &str) -> Option<Number> {
    Number::leading(word).filter(|num| num.to_string() == word)
}

#[cfg(test)]
mod tests;
