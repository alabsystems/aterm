// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bonjour requests a port answers ([`super::System::bonjour`]): on macOS the root LAN broker
//! makes them (`aterm_uds::lan`), so macOS asks nobody whether aterm may use the local network.

use aterm_uds::lan::{Event, LanError};
use std::time::{Duration, Instant};

/// The domain every request names: the link-local one.
pub const DOMAIN: &str = "local.";

/// One Bonjour request in [`DOMAIN`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bonjour {
    /// Browse a service type (`_ssh._tcp`).
    Browse(String),
    /// Resolve the instance `name` of `service`.
    Resolve {
        /// The instance name, exactly as advertised.
        name: String,
        /// Its service type.
        service: String,
    },
    /// Query a record's full name (`<name>._device-info._tcp.local.`) for `rrtype` (16: TXT).
    Query {
        /// The record's full name.
        name: String,
        /// The record type.
        rrtype: u16,
    },
}

impl Bonjour {
    /// The request in one line: what a fake port scripts and records it by.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::Browse(service) => format!("bonjour browse {service}"),
            Self::Resolve { name, service } => format!("bonjour resolve {name:?} {service}"),
            Self::Query { name, rrtype } => format!("bonjour query {name} {rrtype}"),
        }
    }
}

/// What `ask` hears within `within` through `aterm_uds::lan`: every event by then, in order.
/// A request the broker ends early ends the wait with what it heard.
///
/// # Errors
/// One line: no LAN broker to ask (its words), Bonjour off macOS, or the request's own failure
/// before it heard anything.
pub fn live(ask: &Bonjour, within: Duration) -> Result<Vec<Event>, String> {
    let started = match ask {
        Bonjour::Browse(service) => aterm_uds::lan::browse(service, DOMAIN),
        Bonjour::Resolve { name, service } => {
            aterm_uds::lan::resolve_service(name, service, DOMAIN)
        }
        Bonjour::Query { name, rrtype } => aterm_uds::lan::query(name, *rrtype),
    };
    let mut events = started.map_err(|why| why.to_string())?;
    let deadline = Instant::now() + within;
    let mut heard = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match events.next_within(left) {
            Ok(Some(event)) => heard.push(event),
            Ok(None) => break,
            Err(LanError::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(why) if heard.is_empty() => return Err(why.to_string()),
            Err(_) => break,
        }
    }
    Ok(heard)
}

/// The strings of a TXT record's data (each a length byte, then that many bytes); a string
/// cut short ends them.
#[must_use]
pub fn txt_strings(rdata: &[u8]) -> Vec<Vec<u8>> {
    let mut strings = Vec::new();
    let mut rest = rdata;
    while let Some((&len, tail)) = rest.split_first() {
        let Some(string) = tail.get(..usize::from(len)) else {
            break;
        };
        strings.push(string.to_vec());
        rest = &tail[usize::from(len)..];
    }
    strings
}

#[cfg(test)]
#[path = "bonjour_tests.rs"]
mod tests;
