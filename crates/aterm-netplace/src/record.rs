// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The keeper's memory on disk: `<state>/netplace/place.toml`, 0600 in a 0700 directory, written
//! whole by atomic rename and only by the process that holds `keeper.lock` ([`crate::keeper`]).
//! It holds the stays (the current one last), the networks remembered as the ALab LAN, the path
//! monitor's last reading (for a reader that runs no monitor) and who wrote it. Readers never
//! lock it: a rename replaces it whole.
//!
//! The file is a flat TOML subset this module writes and reads back: a schema this build does
//! not know, or a line it cannot read, makes the whole file unread, and the place is worked out
//! afresh (away-since then reads unknown rather than an invented time).

use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use crate::place::{Known, Link, NetworkId, PathFacts, Record, Stay, StayAt};

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

fn path_line(path: &PathFacts) -> String {
    format!(
        "usable={} expensive={} constrained={} link={}",
        match path.usable {
            Some(true) => "1",
            Some(false) => "0",
            None => "-",
        },
        u8::from(path.expensive),
        u8::from(path.constrained),
        path.link.word()
    )
}

fn parse_path(text: &str) -> Option<PathFacts> {
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
    Some(PathFacts {
        usable: usable?,
        expensive: expensive?,
        constrained: constrained?,
        link: link?,
    })
}

fn valid_subnet(subnet: &str) -> bool {
    let Some((addr, prefix)) = subnet.split_once('/') else {
        return false;
    };
    addr.parse::<Ipv4Addr>().is_ok()
        && !prefix.starts_with('+')
        && prefix.parse::<u8>().is_ok_and(|p| p <= 32)
}

fn stay_line(stay: &Stay) -> String {
    let opt = |s: Option<String>| s.unwrap_or_else(|| "-".to_owned());
    format!(
        "{} {} {} {} {} {} {}",
        stay.start_ms,
        opt(stay.end_ms.map(|ms| ms.to_string())),
        stay.at.word(),
        opt(stay.network.as_ref().map(|id| id.as_str().to_owned())),
        opt(stay.subnet.clone()),
        opt(stay.gateway.map(|gw| gw.to_string())),
        u8::from(stay.from_start)
    )
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

fn known_line(known: &Known) -> String {
    format!(
        "{} {} {}",
        known.network.as_str(),
        known.learned_ms,
        known.confirmed_ms
    )
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

/// A TOML basic string of `text`, which holds no quote, backslash or control character.
fn quoted(text: &str) -> String {
    format!("\"{text}\"")
}

/// The record's text.
#[must_use]
pub fn format(written: &Written) -> String {
    let mut out = format!(
        "# aterm's network place: the keeper's memory (aterm-netplace). Rewritten whole; never edit.\n\
         schema = {SCHEMA}\nrevision = {}\n",
        written.revision
    );
    if let Some(pid) = written.keeper_pid {
        out.push_str(&format!("keeper_pid = {pid}\n"));
    }
    if let Some(ms) = written.keeper_started_ms {
        out.push_str(&format!("keeper_started_ms = {ms}\n"));
    }
    out.push_str(&format!("written_ms = {}\n", written.written_ms));
    if let Some(path) = &written.record.path {
        out.push_str(&format!("path = {}\n", quoted(&path_line(path))));
    }
    out.push_str("stays = [\n");
    for stay in &written.record.stays {
        out.push_str(&format!("  {},\n", quoted(&stay_line(stay))));
    }
    out.push_str("]\nremembered = [\n");
    for known in &written.record.remembered {
        out.push_str(&format!("  {},\n", quoted(&known_line(known))));
    }
    out.push_str("]\n");
    out
}

/// The record a [`format`] text holds; `None` for another schema or any line this build cannot
/// read.
#[must_use]
pub fn parse(text: &str) -> Option<Written> {
    let mut schema = None;
    let mut revision = None;
    let mut keeper_pid = None;
    let mut keeper_started_ms = None;
    let mut written_ms = None;
    let mut path = None;
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
            "path" if path.is_none() => {
                path = Some(parse_path(value.strip_prefix('"')?.strip_suffix('"')?)?);
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
            path,
        },
    })
}

/// The record in `dir` ([`dir`]); `None` where it is missing, not private, too large or unread.
#[must_use]
pub fn read(dir: &Path) -> Option<Written> {
    let bytes = aterm_uds::private_file::read_private(dir, FILE, MAX).ok()?;
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

    /// What the keeper writes it reads back whole, and a record of another schema or with one
    /// line it cannot read is unread as a whole, so no half-read memory invents a time.
    /// NEGATIVE CONTROL: passing over a key this build does not know (`_ => {}` in `parse`)
    /// reads the record with the unknown key, and this test fails.
    #[test]
    fn the_record_reads_back_and_an_unknown_one_is_unread_whole() {
        let id = NetworkId::parse("n0123456789abcdef").expect("an id");
        let written = Written {
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
                path: Some(PathFacts {
                    usable: Some(false),
                    expensive: true,
                    constrained: false,
                    link: Link::Wifi,
                }),
            },
        };
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

    /// Truncated files, duplicate keys, malformed `path` booleans, and invalid CIDR `subnet`
    /// tokens make the whole record unread rather than inventing defaults or overwriting fields.
    /// NEGATIVE CONTROL: defaulting missing `revision`/`written_ms`/`stays`/`remembered` or
    /// parsing `expensive=yes` as `false` accepts corrupted `place.toml` inputs, failing this test.
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
                path: Some(PathFacts {
                    usable: Some(true),
                    expensive: false,
                    constrained: false,
                    link: Link::Wired,
                }),
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
        assert_eq!(parse(&base.replace("expensive=0", "expensive=yes")), None);
        assert_eq!(parse(&base.replace("constrained=0", "constrained=2")), None);
        assert_eq!(parse(&base.replace("usable=1", "usable=1 usable=0")), None);
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
