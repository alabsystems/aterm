// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A private native attachment for an ordinary executable's startup measurement.
//! Neither local custody nor the automatic runtime is replaced by the lab.

use std::ffi::OsStr;

pub(super) fn port() -> crate::Result<Option<u16>> {
    parse(
        std::env::var_os(super::LOOPBACK_PORT_ENV).as_deref(),
        std::env::var_os(aterm_net::loopback::SEAM).as_deref(),
    )
}

fn parse(value: Option<&OsStr>, guard: Option<&OsStr>) -> crate::Result<Option<u16>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let port = value
        .to_str()
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|port| *port != 0);
    match (port, guard) {
        (Some(port), Some(guard)) if guard == "1" => Ok(Some(port)),
        _ => Err(crate::Error::new(&format!(
            "{} requires a nonzero port and {}=1; refusing the attachment",
            super::LOOPBACK_PORT_ENV,
            aterm_net::loopback::SEAM,
        ))),
    }
}

#[cfg(test)]
#[path = "isolation_tests.rs"]
mod tests;
