// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Negotiated native-child words of the existing contract-3 protocol.

use std::num::NonZeroU64;

use crate::claim::{self, Child, Contract, NativeChild, Scope, Status};
use crate::config::Config;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    #[default]
    Absent,
    Proof,
    Start,
}

impl Mode {
    pub(crate) fn parse(info: &str) -> Result<Self, &'static str> {
        if info.split_ascii_whitespace().next() != Some("OK") {
            return Err("native child capabilities require an OK info reply");
        }
        let read = |key| match unique(info, key)? {
            None => Ok(false),
            Some("1") => Ok(true),
            Some(_) => Err("invalid present native child capability"),
        };
        let proof = read("child-proof=")?;
        let start = read("child-start=")?;
        match (proof, start) {
            (false, false) => Ok(Self::Absent),
            (true, false) => Ok(Self::Proof),
            (true, true) => Ok(Self::Start),
            (false, true) => Err("child-start requires child-proof"),
        }
    }
}

pub(crate) fn unique<'a>(reply: &'a str, key: &str) -> Result<Option<&'a str>, &'static str> {
    let mut fields = reply
        .split_ascii_whitespace()
        .filter_map(|word| word.strip_prefix(key));
    let value = fields.next();
    if fields.next().is_some() {
        return Err("duplicate native child field");
    }
    Ok(value)
}

/// Extend only a C3 claim with separately observed native data. The metadata's
/// C4 process timestamp is never substituted for the native child identifier.
pub(crate) fn claim(
    config: &Config,
    sid: &str,
    pid: u32,
    metadata: Child<'_>,
    native: Option<NativeChild>,
    mode: Mode,
    generation: Option<NonZeroU64>,
) -> Option<String> {
    let mut verb = claim::claim_verb(config, Contract::Three, sid, pid, metadata)?;
    if mode != Mode::Absent
        && let Some(native) = native
    {
        verb.push_str(&format!(" child={}:{}", native.pid(), native.birth()));
    }
    if let Some(generation) = generation {
        if mode != Mode::Start {
            return None;
        }
        verb.push_str(&format!(" child-start={generation}"));
    }
    Some(verb)
}

pub(crate) fn highest(reply: &str) -> Result<u64, &'static str> {
    Status::parse(reply).map_err(|_| "unreadable child generation status")?;
    unique(reply, "supervisor_child_start=")?
        .and_then(claim::canonical_u64)
        .ok_or("missing or malformed child generation")
}

pub(crate) fn pending(
    reply: &str,
    holder: &str,
    scopes: &[Scope],
    generation: NonZeroU64,
) -> Option<Status> {
    for key in [
        "schema=",
        "supervisor=",
        "supervisor_proven=",
        "supervisor_scope=",
        "supervisor_revision=",
        "supervisor_child_start=",
        "supervisor_child=",
    ] {
        unique(reply, key).ok()??;
    }
    let status = Status::parse(reply).ok()?;
    let named: Vec<Scope> = unique(reply, "supervisor_scope=")
        .ok()??
        .split(',')
        .map(Scope::from_word)
        .collect::<Option<_>>()?;
    let allowed = Contract::Three.scopes(&named);
    if named.is_empty() || allowed.len() != named.len() || allowed != Contract::Three.scopes(scopes)
    {
        return None;
    }
    (status.held_by(holder)
        && scopes.iter().all(|scope| status.takes(*scope))
        && status
            .supervisor_revision
            .is_some_and(|revision| revision > 0)
        && highest(reply).ok() == Some(generation.get())
        && unique(reply, "supervisor_child=").ok()? == Some("-"))
    .then_some(status)
}

/// Lifecycle metadata survives unknown observations, including a pending
/// generation without an observed child. It grants no liveness or authority.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Binding {
    pub(crate) mode: Mode,
    pub(crate) known: Option<NativeChild>,
    pub(crate) generation: Option<NonZeroU64>,
    pub(crate) pending: bool,
}

impl Binding {
    pub(crate) fn generation_for(&self, native: Option<NativeChild>) -> Option<NonZeroU64> {
        self.generation
            .filter(|_| self.known.is_none() || self.known == native)
    }
}

#[cfg(test)]
#[path = "c3_child_tests.rs"]
mod tests;
