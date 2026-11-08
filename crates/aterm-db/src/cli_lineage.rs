// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Public identity metadata from the authenticated, fully retained writer chain.

use crate::{Error, Result};
use aterm_codec::hex::encode_lower;
use aterm_json::{Value, json};
use aterm_secrets::{client::Paths, commons::binding::Binding};
use std::time::Instant;

pub(super) fn run() -> Result<Value> {
    let deadline = crate::admission::deadline();
    let paths = Paths::live().map_err(|_| Error::new("Commons lineage path is unavailable"))?;
    read_at(&paths, deadline)
}

fn read_at(paths: &Paths, deadline: Instant) -> Result<Value> {
    if Instant::now() >= deadline {
        return Err(Error::new("Commons lineage observation expired"));
    }
    let bindings = crate::application_lineage_until(paths, deadline)
        .map_err(|error| error.context("Commons lineage"))?;
    if Instant::now() >= deadline {
        return Err(Error::new("Commons lineage observation expired"));
    }
    let value = report(&bindings)?;
    if Instant::now() >= deadline {
        return Err(Error::new("Commons lineage observation expired"));
    }
    Ok(value)
}

fn report(bindings: &[Binding]) -> Result<Value> {
    let current = bindings
        .last()
        .ok_or_else(|| Error::new("Commons lineage has no current writer"))?;
    Ok(json!({
        "kind": "commons-lineage",
        "scope": "authenticated-application-writer-lineage",
        "order": "predecessor-to-current",
        "bindings": bindings.iter().map(identity).collect::<Vec<_>>(),
        "current": identity(current),
        "grants_authority": false,
    }))
}

fn identity(binding: &Binding) -> Value {
    json!({
        "network": encode_lower(binding.network()),
        "authority": encode_lower(binding.authority()),
        "machine": encode_lower(binding.machine()),
        "incarnation": encode_lower(binding.incarnation()),
        "writer": encode_lower(binding.writer()),
        "actor": encode_lower(&binding.actor()),
        "recipient": binding.recipient().to_bech32(),
    })
}

#[cfg(test)]
#[path = "cli_lineage_tests.rs"]
mod tests;
