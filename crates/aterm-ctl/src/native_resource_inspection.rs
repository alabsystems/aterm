// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one native executable installs its authenticated read-only lookup.
//! A standalone client with no host binding reports unavailable.
use std::{io, sync::OnceLock};

type Lookup = fn(&str) -> io::Result<aterm_json::Value>;
static LOOKUP: OnceLock<Lookup> = OnceLock::new();

/// Install the native host lookup once before command dispatch.
/// # Errors
/// A lookup was already installed; existing authority is never replaced.
pub fn register(lookup: Lookup) -> io::Result<()> {
    LOOKUP.set(lookup).map_err(|_| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "native custody lookup already installed",
        )
    })
}
/// Query installed native coverage without GUI, remote or PATH fallback.
/// # Errors
/// No authenticated native implementation is installed, or its lookup fails.
pub fn lookup(session: &str) -> io::Result<aterm_json::Value> {
    LOOKUP.get().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotConnected,
            "native custody lookup unavailable",
        )
    })?(session)
}

#[cfg(test)]
#[path = "native_resource_inspection_tests.rs"]
mod tests;
