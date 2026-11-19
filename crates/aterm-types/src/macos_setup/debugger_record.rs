// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The account's record of developer mode as the window last saw it on this Mac, beside its
//! `aterm.toml`: `<enabled|disabled> <since> <origin>`, the mode, the wall second the window first
//! saw it, and who set it. The window writes it (`aterm-gui`'s `debugger_access`) and judges from
//! it whether developer mode reading off is the person's choice ([`off_is_chosen`]); a run aterm
//! decided reads the same record (`aterm_machines::init_clt`), so it leaves that choice as the
//! window does.
//!
//! Developer mode is this Mac's, but the folder `aterm.toml` is in travels and may be shared: a
//! dotfiles folder synced live between Macs, or one Migration Assistant carries. So each Mac
//! keeps a record of its own, named by its hardware UUID ([`observed_on`]): a Mac's window
//! replaces only its own file and each reader reads only its own, so another Mac's record says
//! nothing here and no window writes over it, however the folder syncs. Where this Mac's UUID
//! cannot be read there is no record of this Mac: the window writes none, and a run aterm decided
//! reads none.
//!
//! The record from before records named their Mac ([`OBSERVED`]) is left as it stands, for the
//! builds before this one, which still write and read it. The window reads it where this Mac has
//! no record yet and keeps its word, as it did, and writes it once as this Mac's record, so a run
//! aterm decided reads it from then on; a window that cannot read this Mac's UUID reads it and
//! writes nothing.

use std::path::{Path, PathBuf};

/// The record's file name from before records named their Mac: the one builds before them write
/// and read, which this build only reads ([`observed_on`]).
pub const OBSERVED: &str = "privacy-debugger-observed";

/// The file name of the record of the Mac whose hardware UUID is `host` (`gethostuuid(2)`, as
/// `aterm_containment::consent::host_uuid` reads it): `privacy-debugger-observed-<host>`,
/// uppercase. `None` where `host` is not one UUID, so no name is made of anything else.
#[must_use]
pub fn observed_on(host: &str) -> Option<String> {
    let groups: Vec<&str> = host.split('-').collect();
    let uuid = groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit()));
    uuid.then(|| format!("{OBSERVED}-{}", host.to_ascii_uppercase()))
}

/// Where the record of the Mac whose hardware UUID is `host` stands, for the account whose
/// `aterm.toml` is `config`: beside it ([`observed_on`]). `None` where `host` could not be read
/// or is not one UUID.
#[must_use]
pub fn beside_on(config: &Path, host: Option<&str>) -> Option<PathBuf> {
    Some(config.parent()?.join(observed_on(host?)?))
}

/// The record holds one short line; anything bigger is not a record aterm wrote, and reads as
/// none.
pub const MAX_BYTES: usize = 96;

/// Who set the mode a record names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A change aterm made.
    Aterm,
    /// A change made outside aterm after aterm first saw the mode.
    Outside,
    /// The mode as aterm first saw it, set by no one aterm knows.
    Unknown,
}

impl Origin {
    /// The record's word for it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Aterm => "aterm",
            Self::Outside => "outside",
            Self::Unknown => "unknown",
        }
    }

    /// The origin `word` names.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        match word {
            "aterm" => Some(Self::Aterm),
            "outside" => Some(Self::Outside),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

/// One record ([`observed_on`]'s, or [`OBSERVED`]'s from before records named their Mac).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observed {
    /// The mode was on.
    pub enabled: bool,
    /// When aterm first saw this mode, in wall seconds.
    pub since: u64,
    /// Who set it.
    pub origin: Origin,
}

impl Observed {
    /// The record's text: `<enabled|disabled> <since> <origin>`.
    #[must_use]
    pub fn encode(&self) -> String {
        format!(
            "{} {} {}",
            if self.enabled { "enabled" } else { "disabled" },
            self.since,
            self.origin.as_str()
        )
    }

    /// The record `text` holds: its three words, whitespace around them ignored, else `None`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut words = text.split_whitespace();
        let enabled = match words.next()? {
            "enabled" => true,
            "disabled" => false,
            _ => return None,
        };
        let since = words
            .next()
            .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))?
            .parse()
            .ok()?;
        let origin = Origin::from_word(words.next()?)?;
        if words.next().is_some() {
            return None;
        }
        Some(Self {
            enabled,
            since,
            origin,
        })
    }
}

/// Whether developer mode reading off is the person's choice: the reader saw it go off itself
/// (`drift`), or the record says it was on, or that aterm or something else turned it off. Pure,
/// so every launch and every run judges it the same way.
#[must_use]
pub fn off_is_chosen(drift: bool, recorded: Option<&Observed>) -> bool {
    drift || recorded.is_some_and(|was| was.enabled || was.origin != Origin::Unknown)
}

#[cfg(test)]
#[path = "debugger_record_tests.rs"]
mod tests;
