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
    state.join(DIR)
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
mod tests {
    use super::*;

    fn historical() -> Written {
        let id = NetworkId::parse("n0123456789abcdef").expect("an id");
        Written {
            revision: 7,
            keeper_pid: Some(4242),
            keeper_started_ms: Some(1_000),
            written_ms: 2_000,
            record: Record {
                stays: vec![
                    Stay {
                        start_ms: 1_000,
                        end_ms: Some(1_500),
                        at: StayAt::AlabLan,
                        network: Some(id.clone()),
                        subnet: Some("192.168.4.0/22".to_owned()),
                        gateway: Some(Ipv4Addr::new(192, 168, 4, 1)),
                        from_start: true,
                    },
                    Stay {
                        start_ms: 1_500,
                        end_ms: None,
                        at: StayAt::Offline,
                        network: None,
                        subnet: None,
                        gateway: None,
                        from_start: false,
                    },
                ],
                remembered: vec![Known {
                    network: id,
                    learned_ms: 1_100,
                    confirmed_ms: 1_400,
                }],
            },
        }
    }

    /// What the keeper writes it reads back whole, and a record of another schema or with one
    /// line it cannot read is unread as a whole, so no half-read memory invents a time.
    /// NEGATIVE CONTROL: passing over a key this build does not know (`_ => {}` in `parse`)
    /// reads the record with the unknown key, and this test fails.
    #[test]
    fn the_record_reads_back_and_an_unknown_one_is_unread_whole() {
        let written = historical();
        let text = format(&written);
        assert_eq!(parse(&text), Some(written));
        assert_eq!(parse(&text.replace("schema = 1", "schema = 2")), None);
        assert_eq!(parse(&text.replace("alab-lan", "somewhere")), None);
        assert_eq!(
            parse(&text.replace("revision = 7", "revision = 7\nmystery = 1")),
            None,
            "a key this build does not know"
        );
        assert_eq!(
            parse(&text.replace("n0123456789abcdef 1100", "w0123456789abcdef 1100")),
            None,
            "a weak id is never remembered"
        );
        assert_eq!(parse(""), None);
    }

    #[test]
    fn schema_one_memory_survives_without_retaining_its_retired_path() {
        let legacy = "path = \"usable=0 expensive=1 constrained=0 link=wifi\"\n\
            revision = 7\nkeeper_pid = 4242\nkeeper_started_ms = 1000\nwritten_ms = 2000\n\
            stays = [\n\"1000 1500 alab-lan n0123456789abcdef 192.168.4.0/22 192.168.4.1 1\",\n\
            \"1500 - offline - - - 0\",\n]\nschema = 1\n\
            remembered = [\n\"n0123456789abcdef 1100 1400\",\n]\n";
        let expected = historical();
        assert_eq!(parse(legacy), Some(expected.clone()));
        let without_path = legacy.lines().skip(1).collect::<Vec<_>>().join("\n");
        assert_eq!(parse(&without_path), Some(expected.clone()));
        for malformed in [
            legacy.replace("expensive=1", "expensive=yes"),
            legacy.replace("constrained=0", "constrained=2"),
            legacy.replace("usable=0", "usable=0 usable=1"),
            legacy.replace("link=wifi", "link=unknown-link"),
            format!("{legacy}path = \"usable=0 expensive=1 constrained=0 link=wifi\"\n"),
            legacy.replace("schema = 1", "schema = 2"),
            legacy.replace("schema = 1", "schema = 3"),
        ] {
            assert_eq!(parse(&malformed), None);
        }
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = dir(root.path());
        aterm_uds::private_file::ensure_private_dir(&dir).unwrap();
        aterm_uds::private_file::write_private(&dir, FILE, legacy.as_bytes()).unwrap();
        assert_eq!(read(&dir), Some(expected.clone()));
        assert_eq!(
            std::fs::read(dir.join(FILE)).unwrap(),
            legacy.as_bytes(),
            "reading does not force a rewrite"
        );
        write(&dir, &expected).unwrap();
        let rewritten = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(rewritten.lines().any(|line| line == "schema = 1"));
        assert!(!rewritten.lines().any(|line| line.starts_with("path = ")));
        assert_eq!(read(&dir), Some(expected));
    }

    /// Truncated files, duplicate keys and invalid CIDR `subnet`
    /// tokens make the whole record unread rather than inventing defaults or overwriting fields.
    /// NEGATIVE CONTROL: defaulting missing `revision`/`written_ms`/`stays`/`remembered` or
    /// accepting an invalid CIDR accepts corrupted `place.toml` inputs, failing this test.
    #[test]
    fn truncated_duplicate_and_malformed_record_fields_are_unread_whole() {
        let base = format(&Written {
            revision: 3,
            written_ms: 9_000,
            record: Record {
                stays: vec![Stay {
                    start_ms: 1_000,
                    end_ms: None,
                    at: StayAt::AlabLan,
                    network: None,
                    subnet: Some("10.0.0.0/24".to_owned()),
                    gateway: Some(Ipv4Addr::new(10, 0, 0, 1)),
                    from_start: true,
                }],
                remembered: Vec::new(),
            },
            ..Written::default()
        });
        assert!(parse(&base).is_some());
        assert_eq!(parse("schema = 1\n"), None);
        assert_eq!(parse("schema = 1\nrevision = 3\nwritten_ms = 9000\n"), None);
        assert_eq!(
            parse(&base.replace("revision = 3", "revision = 3\nrevision = 4")),
            None
        );
        assert_eq!(
            parse(&base.replace("stays = [", "stays = [\n]\nstays = [")),
            None
        );
        assert_eq!(parse(&base.replace("10.0.0.0/24", "10.0.0.0/33")), None);
        assert_eq!(parse(&base.replace("10.0.0.0/24", "not-a-cidr")), None);
    }

    /// The record is written private and read back through the private-file rules.
    /// NEGATIVE CONTROL: writing it with a plain `std::fs::write` (umask mode) leaves it
    /// unreadable through the private-file rules, and this test fails.
    #[test]
    fn the_record_is_written_private_and_read_back() {
        let root = aterm_tempfile::tempdir().expect("a scratch directory");
        let dir = dir(root.path());
        assert_eq!(read(&dir), None, "nothing written yet");
        let written = Written {
            revision: 1,
            written_ms: 5,
            ..Written::default()
        };
        write(&dir, &written).expect("written");
        assert_eq!(read(&dir), Some(written));
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(dir.join(FILE))
            .expect("the file")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let mode = std::fs::metadata(&dir)
            .expect("the dir")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }
}
