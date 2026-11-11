// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The keeper's memory on disk: `<state>/netplace/place.toml`, 0600 in a 0700 directory, written
//! whole by atomic rename and only by the process that holds `keeper.lock` ([`crate::keeper`]).
//! It holds the stays (the current one last), the networks remembered as the ALab LAN
//! and who wrote it. Readers never
//! lock it: a rename replaces it whole.
//!
//! The file is a flat TOML subset this module writes and reads back: a schema this build does
//! not know, or a line it cannot read, makes the whole file unread, and the place is worked out
//! afresh (away-since then reads unknown rather than an invented time).
//! Schema 1's optional retired path field is validated and discarded on read, and omitted on
//! write. The format already permits that omission, so history remains readable by existing
//! processes. Reading never forces a rewrite or discards saved stays and remembered networks.

use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use crate::place::{Known, Link, NetworkId, Record, Stay, StayAt};

/// The directory under the state root.
pub const DIR: &str = "netplace";

/// The record's file name.
pub const FILE: &str = "place.toml";

/// The schema this build writes and reads.
pub const SCHEMA: u32 = 1;

/// The most bytes read from the record.
const MAX: usize = 64 * 1024;

/// The place's directory under `state` (aterm's state root).
#[must_use]
pub fn dir(state: &Path) -> PathBuf {
    aterm_uds::quiet_state::selected(&state.join(DIR))
}

/// The record as written: the keeper's memory and who wrote it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Written {
    /// Bumped on every write.
    pub revision: u64,
    /// The writing keeper's pid.
    pub keeper_pid: Option<u32>,
    /// When that keeper started (unix ms).
    pub keeper_started_ms: Option<u64>,
    /// When it was written (unix ms).
    pub written_ms: u64,
    /// The memory.
    pub record: Record,
}

/// Schema 1 retained monitor state. Validate that retired field so migration preserves the
/// historical memory without accepting malformed records or constructing current path facts.
fn validate_legacy_path(text: &str) -> Option<()> {
    let mut usable = None;
    let mut expensive = None;
    let mut constrained = None;
    let mut link = None;
    for part in text.split(' ') {
        let (key, value) = part.split_once('=')?;
        match key {
            "usable" if usable.is_none() => {
                usable = Some(match value {
                    "1" => Some(true),
                    "0" => Some(false),
                    "-" => None,
                    _ => return None,
                });
            }
            "expensive" if expensive.is_none() => {
                expensive = Some(match value {
                    "1" => true,
                    "0" => false,
                    _ => return None,
                });
            }
            "constrained" if constrained.is_none() => {
                constrained = Some(match value {
                    "1" => true,
                    "0" => false,
                    _ => return None,
                });
            }
            "link" if link.is_none() => link = Some(Link::from_word(value)?),
            _ => return None,
        }
    }
    (usable.is_some() && expensive.is_some() && constrained.is_some() && link.is_some())
        .then_some(())
}

fn valid_subnet(subnet: &str) -> bool {
    let Some((addr, prefix)) = subnet.split_once('/') else {
        return false;
    };
    addr.parse::<Ipv4Addr>().is_ok()
        && !prefix.starts_with('+')
        && prefix.parse::<u8>().is_ok_and(|p| p <= 32)
}

fn write_stay_line(out: &mut String, stay: &Stay) {
    use std::fmt::Write as _;
    let _ = write!(out, "{} ", stay.start_ms);
    match stay.end_ms {
        Some(ms) => {
            let _ = write!(out, "{ms} ");
        }
        None => out.push_str("- "),
    }
    let _ = write!(
        out,
        "{} {} {} ",
        stay.at.word(),
        stay.network.as_ref().map_or("-", NetworkId::as_str),
        stay.subnet.as_deref().unwrap_or("-"),
    );
    match stay.gateway {
        Some(gw) => {
            let _ = write!(out, "{gw} ");
        }
        None => out.push_str("- "),
    }
    let _ = write!(out, "{}", u8::from(stay.from_start));
}

fn parse_stay(text: &str) -> Option<Stay> {
    let mut words = text.split(' ');
    let mut next = || words.next();
    let start_ms = next()?.parse().ok()?;
    let end_ms = match next()? {
        "-" => None,
        ms => Some(ms.parse().ok()?),
    };
    let at = StayAt::from_word(next()?)?;
    let network = match next()? {
        "-" => None,
        id => Some(NetworkId::parse(id)?),
    };
    let subnet = match next()? {
        "-" => None,
        subnet if valid_subnet(subnet) => Some(subnet.to_owned()),
        _ => return None,
    };
    let gateway = match next()? {
        "-" => None,
        gw => Some(gw.parse::<Ipv4Addr>().ok()?),
    };
    let from_start = match next()? {
        "1" => true,
        "0" => false,
        _ => return None,
    };
    next().is_none().then_some(Stay {
        start_ms,
        end_ms,
        at,
        network,
        subnet,
        gateway,
        from_start,
    })
}

fn parse_known(text: &str) -> Option<Known> {
    let mut words = text.split(' ');
    let network = NetworkId::parse(words.next()?).filter(NetworkId::is_strong)?;
    let learned_ms = words.next()?.parse().ok()?;
    let confirmed_ms = words.next()?.parse().ok()?;
    words.next().is_none().then_some(Known {
        network,
        learned_ms,
        confirmed_ms,
    })
}

/// The record's text.
#[must_use]
pub fn format(written: &Written) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(
        256 + written.record.stays.len().saturating_mul(72)
            + written.record.remembered.len().saturating_mul(48),
    );
    let _ = write!(
        out,
        "# aterm's network place: the keeper's memory (aterm-netplace). Rewritten whole; never edit.\n\
         schema = {SCHEMA}\nrevision = {}\n",
        written.revision
    );
    if let Some(pid) = written.keeper_pid {
        let _ = writeln!(out, "keeper_pid = {pid}");
    }
    if let Some(ms) = written.keeper_started_ms {
        let _ = writeln!(out, "keeper_started_ms = {ms}");
    }
    let _ = writeln!(out, "written_ms = {}", written.written_ms);
    out.push_str("stays = [\n");
    for stay in &written.record.stays {
        out.push_str("  \"");
        write_stay_line(&mut out, stay);
        out.push_str("\",\n");
    }
    out.push_str("]\nremembered = [\n");
    for known in &written.record.remembered {
        let _ = writeln!(
            out,
            "  \"{} {} {}\",",
            known.network.as_str(),
            known.learned_ms,
            known.confirmed_ms
        );
    }
    out.push_str("]\n");
    out
}

/// The record a [`format`] text holds, validating and discarding its optional retired path.
/// Unknown schemas or malformed fields make the whole record unread.
#[must_use]
pub fn parse(text: &str) -> Option<Written> {
    let mut schema = None;
    let mut revision = None;
    let mut keeper_pid = None;
    let mut keeper_started_ms = None;
    let mut written_ms = None;
    let mut legacy_path_seen = false;
    let mut stays: Option<Vec<Stay>> = None;
    let mut remembered: Option<Vec<Known>> = None;
    let mut list: Option<&str> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = list {
            if line == "]" {
                list = None;
                continue;
            }
            let item = line
                .strip_suffix(',')
                .unwrap_or(line)
                .strip_prefix('"')?
                .strip_suffix('"')?;
            match name {
                "stays" => stays.as_mut()?.push(parse_stay(item)?),
                _ => remembered.as_mut()?.push(parse_known(item)?),
            }
            continue;
        }
        let (key, value) = line.split_once(" = ")?;
        match key {
            "schema" if schema.is_none() => schema = Some(value.parse::<u32>().ok()?),
            "revision" if revision.is_none() => revision = Some(value.parse().ok()?),
            "keeper_pid" if keeper_pid.is_none() => keeper_pid = Some(value.parse().ok()?),
            "keeper_started_ms" if keeper_started_ms.is_none() => {
                keeper_started_ms = Some(value.parse().ok()?);
            }
            "written_ms" if written_ms.is_none() => written_ms = Some(value.parse().ok()?),
            "path" if !legacy_path_seen => {
                validate_legacy_path(value.strip_prefix('"')?.strip_suffix('"')?)?;
                legacy_path_seen = true;
            }
            "stays" if value == "[" && stays.is_none() => {
                stays = Some(Vec::new());
                list = Some("stays");
            }
            "remembered" if value == "[" && remembered.is_none() => {
                remembered = Some(Vec::new());
                list = Some("remembered");
            }
            _ => return None,
        }
    }
    if schema != Some(SCHEMA) || list.is_some() {
        return None;
    }
    Some(Written {
        revision: revision?,
        keeper_pid,
        keeper_started_ms,
        written_ms: written_ms?,
        record: Record {
            stays: stays?,
            remembered: remembered?,
        },
    })
}

/// The record in `dir` ([`dir`]); `None` where it is missing, not private, too large or unread.
#[must_use]
pub fn read(dir: &Path) -> Option<Written> {
    let judged = aterm_uds::private_file::JudgedDir::judge(dir).ok()?;
    read_in(&judged)
}

pub(crate) fn read_in(judged: &aterm_uds::private_file::JudgedDir<'_>) -> Option<Written> {
    let bytes = judged.read(FILE, MAX).ok()?;
    parse(std::str::from_utf8(&bytes).ok()?)
}

/// Writes `written` into `dir` whole, by atomic rename, making `dir` private (0700) first.
///
/// # Errors
/// The directory could not be made private or the file could not be written.
pub fn write(dir: &Path, written: &Written) -> io::Result<()> {
    aterm_uds::private_file::ensure_private_dir(dir)?;
    aterm_uds::private_file::write_private(dir, FILE, format(written).as_bytes())
}

#[cfg(test)]
#[path = "record_tests.rs"]
mod tests;
