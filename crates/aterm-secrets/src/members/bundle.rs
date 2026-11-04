// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the Allowing member hands the new Mac inside the CPace channel (§3.3.4 step 9): the
//! vault's name, its pinned signer and the keys that pin was reached through, the servers the
//! member reads it from, the member's signed [`Allowed`], and then one of two payloads
//! ([`Handed`]):
//!
//! - **The membership log** (`alab-join-bundle/3`, F15), where the member's vault has one and
//!   the member holds a seat and its epochs: the id of the `Admit` the member signed for the new
//!   Mac, and the member's complete record history, from the genesis through that `Admit` and its
//!   `Wrap`s, with the source's capacity and crossed state. The new Mac checks the path to the
//!   genesis and reads its files from the log. Version two carries no source bound and is
//!   reserved for explicitly negotiated legacy readers and uncrossed history.
//! - **The member's `[everyone]` files** (`alab-join-bundle/1`, the F10 interim), as locker
//!   text, everywhere else: a vault with no log (ALab until F12b), or a member with no seat in
//!   it. An admin's approver then records the admission.

use super::Allowed;
use crate::crossing::RecordsBound;
use crate::membership_log::{MAX_RECORDS_STREAM, RecordId};
use crate::{Error, Result, SecretBytes};

const HEADER: &str = "alab-join-bundle/1";
const HEADER_LOG: &str = "alab-join-bundle/2";
const HEADER_BOUNDED_LOG: &str = "alab-join-bundle/3";
/// The largest bundle either side handles, sealed or opened: a whole record store
/// ([`MAX_RECORDS_STREAM`]) and its header lines.
pub const MAX_BUNDLE: usize = MAX_RECORDS_STREAM + (1 << 16);
/// The largest `[everyone]` files payload, F10's bound.
const MAX_FILES: usize = 1 << 20;
const MAX_SERVERS: usize = 8;
const MAX_FOLLOWED: usize = 64;

/// The joining device's explicit payload and capacity capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reader {
    /// An older device asks for the files payload and cannot take a membership log.
    Files,
    /// Version two reads a complete, uncrossed legacy log.
    LegacyLog,
    /// Version three states its own capacity and crossed history.
    BoundedLog(RecordsBound),
}

impl Reader {
    pub(crate) const fn reads_log(self) -> bool {
        !matches!(self, Self::Files)
    }

    pub(crate) const fn can_take(self, source: RecordsBound) -> bool {
        match self {
            Self::Files => false,
            Self::LegacyLog => source.held != 0 && !source.crossed,
            Self::BoundedLog(peer) => peer.can_exchange(source),
        }
    }

    /// Why the allowing member hands this reader no log of `source`, a history it cannot take
    /// ([`Self::can_take`]), naming the Mac whose aterm must be updated: the new Mac where it
    /// reads no log, reads only an uncrossed one (a build before the crossed fence) or holds
    /// fewer records than `source` may; else the allowing Mac, whose bound is short of the
    /// history the new Mac states (a crossed one, where the member holds at least
    /// [`crate::membership_log::LEGACY_RECORDS`]). The member says it to the person and on the wire, where an
    /// older new Mac shows it as the member's reason.
    pub(crate) fn refusal(self, source: RecordsBound) -> String {
        match self {
            Self::Files => "the new Mac's aterm reads no membership log: update aterm on the new \
                            Mac, then ask again"
                .to_owned(),
            Self::LegacyLog => format!(
                "this vault's membership log has passed the {} records an older aterm takes, and \
                 the new Mac runs one: update aterm on the new Mac, then ask again",
                crate::membership_log::LEGACY_RECORDS
            ),
            // Only one side can be short: the new Mac of a history up to `source.held`, or the
            // member of one up to `peer.held`.
            Self::BoundedLog(peer) if peer.held < source.held => format!(
                "this vault's membership log may hold {} records, more than the {} the new Mac's \
                 aterm takes: update aterm on the new Mac, then ask again",
                source.held, peer.held
            ),
            Self::BoundedLog(_) => format!(
                "the new Mac's membership log may hold more records than the {} the allowing \
                 Mac's aterm takes: update aterm on the allowing Mac, then ask again",
                source.held
            ),
        }
    }
}

/// Why a new Mac holding `local` takes no member's log bundle that stated `source` (`None` for
/// `alab-join-bundle/2`, which only a member before the crossed fence hands for a joiner that
/// states a bound), naming the Mac whose aterm must be updated: the mirror of
/// [`Reader::refusal`], seen from the new Mac. `None` where it takes it.
pub(crate) fn source_refusal(local: RecordsBound, source: Option<RecordsBound>) -> Option<String> {
    match source {
        None if local.crossed => Some(
            "this Mac's membership log has crossed past the records the allowing Mac's older \
             aterm takes: update aterm on the allowing Mac, then ask again"
                .to_owned(),
        ),
        Some(source) if !local.can_exchange(source) && local.held < source.held => Some(format!(
            "the allowing Mac's membership log may hold {} records, more than the {} this Mac's \
             aterm takes: update aterm on this Mac, then ask again",
            source.held, local.held
        )),
        Some(source) if !local.can_exchange(source) => Some(format!(
            "this Mac's membership log may hold more records than the {} the allowing Mac's aterm \
             takes: update aterm on the allowing Mac, then ask again",
            source.held
        )),
        _ => None,
    }
}

/// One join bundle. Its files are wiped with it.
pub struct Bundle {
    /// The vault's name.
    pub vault: String,
    /// Its pinned signing key.
    pub signer: [u8; 32],
    /// The keys `signer` was reached through, oldest first.
    pub followed: Vec<[u8; 32]>,
    /// The URLs the member reads the vault from, first the one it is configured with.
    pub servers: Vec<String>,
    /// The member's Allow of the new Mac.
    pub allowed: Allowed,
    /// What lets the new Mac read the vault.
    pub handed: Handed,
}

/// A bundle's payload ([`Bundle`]).
pub enum Handed {
    /// The F10 interim: the member's `[everyone]` entries, as one `[everyone]` locker's text.
    Files(SecretBytes),
    /// F15: the member's `Admit` of the new Mac, and every complete record it holds (that
    /// `Admit` and its `Wrap`s included), as one record stream.
    Log {
        /// The source's capacity and crossed state; `None` is the legacy version-two wire.
        source: Option<RecordsBound>,
        /// The `Admit`'s id.
        admit: RecordId,
        /// The records, genesis first ([`crate::membership_log::Store::encode`]).
        records: Vec<u8>,
    },
}

impl Handed {
    /// The bundle's header line for this payload.
    const fn header(&self) -> &'static str {
        match self {
            Handed::Files(_) => HEADER,
            Handed::Log { source: None, .. } => HEADER_LOG,
            Handed::Log {
                source: Some(_), ..
            } => HEADER_BOUNDED_LOG,
        }
    }
}

impl std::fmt::Debug for Bundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bundle")
            .field("vault", &self.vault)
            .field("servers", &self.servers)
            .finish_non_exhaustive()
    }
}

fn key_text(key: &[u8; 32]) -> String {
    crate::b64::encode_padded(key)
}

fn key_of(text: &str) -> Result<[u8; 32]> {
    crate::b64::decode_padded(text)
        .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
        .ok_or_else(|| Error::new("a join bundle's key is not 32 bytes of base64"))
}

fn server_ok(url: &str) -> bool {
    (url.starts_with("http://") || url.starts_with("https://"))
        && url.len() <= 512
        && url.bytes().all(|b| b.is_ascii_graphic())
}

impl Bundle {
    /// The `[everyone]` files it hands, for an F10 interim bundle ([`Handed::Files`]).
    #[must_use]
    pub const fn files(&self) -> Option<&SecretBytes> {
        match &self.handed {
            Handed::Files(files) => Some(files),
            Handed::Log { .. } => None,
        }
    }

    /// Its text: a header, one `key value` line per field, then `files` and the locker text, or
    /// a version-three `records-bound` line, `admit <id>`, `records` and the record stream.
    /// A legacy version-two log bundle omits the bound line.
    ///
    /// # Errors
    /// The bundle would exceed [`MAX_BUNDLE`], a field cannot be written on one line, or the
    /// source records bound is invalid.
    pub fn encode(&self) -> Result<SecretBytes> {
        if !crate::envelope::valid_name(&self.vault)
            || self.servers.is_empty()
            || self.servers.len() > MAX_SERVERS
            || !self.servers.iter().all(|url| server_ok(url))
            || self.followed.len() > MAX_FOLLOWED
        {
            return Err(Error::new("a join bundle's fields do not fit its lines"));
        }
        let mut head = format!(
            "{}\nvault {}\nsigner {}\n",
            self.handed.header(),
            self.vault,
            key_text(&self.signer)
        );
        for key in &self.followed {
            head.push_str(&format!("followed {}\n", key_text(key)));
        }
        for url in &self.servers {
            head.push_str(&format!("server {url}\n"));
        }
        head.push_str(&format!("allowed {}\n", self.allowed.to_text()));
        let body: &[u8] = match &self.handed {
            Handed::Files(files) => {
                if files.as_slice().len() > MAX_FILES {
                    return Err(Error::new("this Mac's files exceed a join bundle's bound"));
                }
                head.push_str("files\n");
                files.as_slice()
            }
            Handed::Log {
                source,
                admit,
                records,
            } => {
                if records.len() > MAX_RECORDS_STREAM {
                    return Err(Error::new(
                        "this Mac's records exceed a join bundle's bound",
                    ));
                }
                if let Some(source) = source {
                    let value = source.value();
                    if RecordsBound::parse(&value) != Some(*source) {
                        return Err(Error::new("this Mac's records bound is invalid"));
                    }
                    head.push_str(&format!("records-bound {value}\n"));
                }
                head.push_str(&format!("admit {}\nrecords\n", super::wire::hex(admit)));
                records.as_slice()
            }
        };
        let mut out = head.into_bytes();
        out.extend_from_slice(body);
        Ok(SecretBytes::new(out))
    }

    /// Read a bundle's text.
    ///
    /// # Errors
    /// It is larger than [`MAX_BUNDLE`], or any line is not the one its place names.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let bad = || Error::new("the member's join bundle is malformed");
        if bytes.len() > MAX_BUNDLE {
            return Err(Error::new("the member's join bundle exceeds its bound"));
        }
        let mut rest = bytes;
        let mut line = || -> Result<&str> {
            let at = rest.iter().position(|b| *b == b'\n').ok_or_else(bad)?;
            let (head, tail) = rest.split_at(at);
            rest = &tail[1..];
            std::str::from_utf8(head).map_err(|_| bad())
        };
        let (log, bounded) = match line()? {
            HEADER => (false, false),
            HEADER_LOG => (true, false),
            HEADER_BOUNDED_LOG => (true, true),
            _ => return Err(bad()),
        };
        let vault = line()?
            .strip_prefix("vault ")
            .filter(|name| crate::envelope::valid_name(name))
            .ok_or_else(bad)?
            .to_owned();
        let signer = key_of(line()?.strip_prefix("signer ").ok_or_else(bad)?)?;
        let mut followed = Vec::new();
        let mut servers = Vec::new();
        let allowed = loop {
            let next = line()?;
            if let Some(key) = next.strip_prefix("followed ") {
                if !servers.is_empty() || followed.len() >= MAX_FOLLOWED {
                    return Err(bad());
                }
                followed.push(key_of(key)?);
            } else if let Some(url) = next.strip_prefix("server ") {
                if !server_ok(url) || servers.len() >= MAX_SERVERS {
                    return Err(bad());
                }
                servers.push(url.to_owned());
            } else if let Some(text) = next.strip_prefix("allowed ") {
                break Allowed::from_text(text)?;
            } else {
                return Err(bad());
            }
        };
        if servers.is_empty() {
            return Err(bad());
        }
        let handed = if log {
            let source = if bounded {
                Some(
                    line()?
                        .strip_prefix("records-bound ")
                        .and_then(RecordsBound::parse)
                        .ok_or_else(bad)?,
                )
            } else {
                None
            };
            let admit = line()?
                .strip_prefix("admit ")
                .and_then(super::wire::unhex::<32>)
                .ok_or_else(bad)?;
            if line()? != "records" || rest.len() > MAX_RECORDS_STREAM {
                return Err(bad());
            }
            Handed::Log {
                source,
                admit,
                records: rest.to_vec(),
            }
        } else {
            if line()? != "files" || rest.len() > MAX_FILES {
                return Err(bad());
            }
            Handed::Files(SecretBytes::new(rest.to_vec()))
        };
        Ok(Self {
            vault,
            signer,
            followed,
            servers,
            allowed,
            handed,
        })
    }
}
