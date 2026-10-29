// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the Allowing member hands the new Mac inside the CPace channel (§3.3.4 step 9): the
//! vault's name, its pinned signer and the keys that pin was reached through, the servers the
//! member reads it from, the member's signed [`Allowed`], and then one of two payloads
//! ([`Handed`]):
//!
//! - **The membership log** (`alab-join-bundle/2`, F15), where the member's vault has one and
//!   the member holds a seat and its epochs: the id of the `Admit` the member signed for the new
//!   Mac, and the member's whole record store, from the genesis through that `Admit` and its
//!   `Wrap`s. The new Mac checks the path to the genesis and reads its files from the log.
//! - **The member's `[everyone]` files** (`alab-join-bundle/1`, the F10 interim), as locker
//!   text, everywhere else: a vault with no log (ALab until F12b), or a member with no seat in
//!   it. An admin's approver then records the admission.

use super::Allowed;
use crate::membership_log::{MAX_RECORDS_STREAM, RecordId};
use crate::{Error, Result, SecretBytes};

const HEADER: &str = "alab-join-bundle/1";
const HEADER_LOG: &str = "alab-join-bundle/2";
/// The largest bundle either side handles, sealed or opened: a whole record store
/// ([`MAX_RECORDS_STREAM`]) and its header lines.
pub const MAX_BUNDLE: usize = MAX_RECORDS_STREAM + (1 << 16);
/// The largest `[everyone]` files payload, F10's bound.
const MAX_FILES: usize = 1 << 20;
const MAX_SERVERS: usize = 8;
const MAX_FOLLOWED: usize = 64;

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
    /// F15: the member's `Admit` of the new Mac, and every record the member holds (that
    /// `Admit` and its `Wrap`s included), as one record stream.
    Log {
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
            Handed::Log { .. } => HEADER_LOG,
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
    /// `admit <id>`, `records` and the record stream.
    ///
    /// # Errors
    /// The bundle would exceed [`MAX_BUNDLE`], or a field cannot be written on one line.
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
            Handed::Log { admit, records } => {
                if records.len() > MAX_RECORDS_STREAM {
                    return Err(Error::new(
                        "this Mac's records exceed a join bundle's bound",
                    ));
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
        let log = match line()? {
            HEADER => false,
            HEADER_LOG => true,
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
            let admit = line()?
                .strip_prefix("admit ")
                .and_then(super::wire::unhex::<32>)
                .ok_or_else(bad)?;
            if line()? != "records" || rest.len() > MAX_RECORDS_STREAM {
                return Err(bad());
            }
            Handed::Log {
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
