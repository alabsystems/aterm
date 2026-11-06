// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fleet seed (`docs/DESIGN-machine-names-2026-11-04.md` §3.3): 16 random bytes the fleet's
//! root mints once, signed with the minting node, the mint time and the mark of the fleet's
//! secret, which every other member adopts and never mints. A name record is signed under its
//! digest (§3.2), so a record from another fleet does not verify here, and the machine anchor is
//! keyed by it (§6.4). The mark binds the seed to its fleet: the same root's seed of a fleet it
//! founded before verifies under no key of this one, and a rotate re-signs the seed it keeps.
//!
//! `<state>/fleet-seed` holds the one this node holds, written under the book's lock
//! ([`super::HeldBook::keep_seed`]); a leave removes it and keeps its digest in
//! [`DIGEST_FILE`] beside the node key ([`super::Leaving::keep_seed_digest`]). A seed
//! a member's book reply relays is adopted under the same lock ([`super::HeldBook::adopt_relayed_seed`]),
//! and the last one refused as another minter's is recorded in [`CONFLICT_FILE`]. A root that
//! holds none mints after [`UNHELD_WAIT_MS`] ([`super::HeldBook::mint_unheld_seed`]).

use std::io;
use std::path::Path;

use aterm_types::machine_name::NodeId;

use super::name_record::{Seed, node_of, node_word, seed_digest};
use crate::fleet::{FleetMark, NodeIdentity, NodeKey, SIG_LEN, verifies_record};

/// The seed's file in the state directory.
pub const FILE: &str = "fleet-seed";

/// The digest of the seed held before the last leave.
pub const DIGEST_FILE: &str = "fleet-seed-digest";

/// The record of the last seed this node refused as another minter's than the one it holds, or,
/// on a root, than itself (`seed-conflict`, [`Judged::Conflict`]): its digest, minter and mint time and when it was
/// refused, never the seed, kept for a reader to ask about, not raised.
pub const CONFLICT_FILE: &str = "fleet-seed-conflict";

/// When a root first found it held no seed (ms, one line), which it mints once
/// [`UNHELD_WAIT_MS`] has passed since ([`super::HeldBook::mint_unheld_seed`]).
pub const UNHELD_FILE: &str = "fleet-seed-unheld";

/// How long a root holding no seed reads its members' books, which relay the one it minted
/// before if any member holds it, before it mints: §3.3's ten 60-second reconcile ticks, which its
/// heartbeat rounds (every 3 minutes) stand in for until `reconcile` is built.
pub const UNHELD_WAIT_MS: u64 = 10 * 60_000;

/// What the minter's signature is made under.
pub const DOMAIN: &str = "aterm/fleet-seed/1";

/// The most bytes the seed's line takes: `seed=` 32 hex, ` seed_by=` a 32-hex node id,
/// ` seed_ms=` 20 digits, ` seed_sig=` 128 hex, a newline.
pub const LINE_MAX: usize = 5 + 32 + 9 + 32 + 9 + 20 + 10 + 2 * SIG_LEN + 1;

/// A fleet seed as its minter signed it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct FleetSeed {
    seed: Seed,
    minter: NodeId,
    minted_ms: u64,
    sig: [u8; SIG_LEN],
}

/// What a node holding `held` makes of a seed offered to it ([`judge`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Judged {
    /// It holds none: the first seed is taken on trust (§12.2).
    Take,
    /// It holds this one, or one its minter minted earlier.
    Keep,
    /// Its own minter minted it earlier (at one mint time, the lower digest): a root's second
    /// mint settles on its first, at one re-sign per node.
    Earlier,
    /// Another minter's: refused and recorded as `seed-conflict`, so no member re-keys a fleet
    /// by minting or relaying a seed.
    Conflict,
    /// The seed it holds, signed again (a rotate's or a rekey's re-sign under the new secret):
    /// the holder keeps the offered signature, which verifies under the fleet's key now.
    Resigned,
}

impl FleetSeed {
    /// A new seed `me` mints at `now_ms` for the fleet whose secret's mark is `mark`, from the
    /// OS's CSPRNG.
    ///
    /// # Errors
    /// The CSPRNG cannot be read, or `me`'s node id is no book id.
    pub fn mint(me: &NodeIdentity, mark: FleetMark, now_ms: u64) -> io::Result<Self> {
        let minter = node_of(me.node())
            .ok_or_else(|| io::Error::other(format!("{} is no node id", me.node())))?;
        let mut seed = [0; 16];
        aterm_uds::rand::fill(&mut seed)?;
        let sig = me.sign_record(DOMAIN, &body(&seed, minter, now_ms, mark));
        Ok(Self {
            seed,
            minter,
            minted_ms: now_ms,
            sig,
        })
    }

    /// This seed signed again by its minter `me` for the fleet's new secret, whose mark is
    /// `mark` (a rotate, a rekey): the same seed and mint time. `None` when `me` did not mint it.
    #[must_use]
    pub fn resign(&self, me: &NodeIdentity, mark: FleetMark) -> Option<Self> {
        (node_of(me.node()) == Some(self.minter)).then(|| Self {
            sig: me.sign_record(DOMAIN, &body(&self.seed, self.minter, self.minted_ms, mark)),
            ..*self
        })
    }

    /// The seed.
    #[must_use]
    pub const fn seed(&self) -> &Seed {
        &self.seed
    }

    /// Its digest, which name records are signed under (§3.2).
    #[must_use]
    pub fn digest(&self) -> [u8; 8] {
        seed_digest(&self.seed)
    }

    /// The node that minted it.
    #[must_use]
    pub const fn minter(&self) -> NodeId {
        self.minter
    }

    /// When it was minted (ms), as its minter signed.
    #[must_use]
    pub const fn minted_ms(&self) -> u64 {
        self.minted_ms
    }

    /// Whether its minter's signature holds under `key`, the key the reader's book binds to
    /// [`FleetSeed::minter`], for the fleet whose secret's mark is `mark`, the key the reader
    /// holds.
    #[must_use]
    pub fn verifies(&self, key: &NodeKey, mark: FleetMark) -> bool {
        verifies_record(
            key,
            DOMAIN,
            &body(&self.seed, self.minter, self.minted_ms, mark),
            &self.sig,
        )
    }

    /// Its words: `seed=<32 hex> seed_by=<node> seed_ms=<ms> seed_sig=<128 hex>`, as the file,
    /// a join reply and a book reply's trailer carry it.
    #[must_use]
    pub fn words(&self) -> String {
        format!(
            "seed={} seed_by={} seed_ms={} seed_sig={}",
            aterm_codec::hex::encode_lower(&self.seed),
            node_word(self.minter),
            self.minted_ms,
            aterm_codec::hex::encode_lower(&self.sig),
        )
    }

    /// The seed `line`'s words spell, in any order among other words, each once; `None` when
    /// one is missing, repeated or outside its grammar. Its signature is not checked
    /// ([`FleetSeed::verifies`]).
    #[must_use]
    pub fn of_line(line: &str) -> Option<Self> {
        let mut fields: [Option<&str>; 4] = [None; 4];
        for word in line.split_ascii_whitespace() {
            let Some((key, value)) = word.split_once('=') else {
                continue;
            };
            let at = match key {
                "seed" => 0,
                "seed_by" => 1,
                "seed_ms" => 2,
                "seed_sig" => 3,
                _ => continue,
            };
            if fields[at].replace(value).is_some() {
                return None;
            }
        }
        let [Some(seed), Some(by), Some(ms), Some(sig)] = fields else {
            return None;
        };
        let seed = lower_hex(seed)?;
        let sig = lower_hex(sig)?;
        let minted_ms = (ms.len() <= 20 && ms.bytes().all(|b| b.is_ascii_digit()))
            .then(|| ms.parse().ok())
            .flatten()?;
        Some(Self {
            seed,
            minter: node_of(by)?,
            minted_ms,
            sig,
        })
    }
}

impl std::fmt::Debug for FleetSeed {
    /// The digest, minter and mint time, never the seed: the anchor key derives from it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FleetSeed")
            .field("digest", &aterm_codec::hex::encode_lower(&self.digest()))
            .field("minter", &node_word(self.minter))
            .field("minted_ms", &self.minted_ms)
            .finish_non_exhaustive()
    }
}

/// What a node holding `held` makes of `offered`, a seed whose signature already holds under
/// its minter's bound key and the mark of the key this node holds (§3.3): a node replaces its
/// seed only with one of the same minter and an earlier signed mint time, and at one mint time
/// with the lower digest, so every member settles on one; the seed it holds signed again it
/// keeps under the new signature. A join, which takes the new fleet's seed whatever it held,
/// does not ask.
#[must_use]
pub fn judge(held: Option<&FleetSeed>, offered: &FleetSeed) -> Judged {
    let Some(held) = held else {
        return Judged::Take;
    };
    if held.minter != offered.minter {
        Judged::Conflict
    } else if (offered.seed, offered.minted_ms) == (held.seed, held.minted_ms) {
        if offered.sig == held.sig {
            Judged::Keep
        } else {
            Judged::Resigned
        }
    } else if (offered.minted_ms, offered.digest()) < (held.minted_ms, held.digest()) {
        Judged::Earlier
    } else {
        Judged::Keep
    }
}

/// The seed `dir` holds, `None` when there is none.
///
/// # Errors
/// It cannot be read privately, or it is no seed's line.
pub fn read(dir: &Path) -> io::Result<Option<FleetSeed>> {
    let Some(text) = read_text(dir, FILE)? else {
        return Ok(None);
    };
    FleetSeed::of_line(&text)
        .map(Some)
        .ok_or_else(|| malformed(FILE, "holds no fleet seed"))
}

/// The digest of the seed this node held before its last leave, `None` when there is none.
///
/// # Errors
/// It cannot be read privately, or it is no 16-digit lowercase hex line.
pub fn read_digest(dir: &Path) -> io::Result<Option<[u8; 8]>> {
    let Some(text) = read_text(dir, DIGEST_FILE)? else {
        return Ok(None);
    };
    lower_hex(text.strip_suffix('\n').unwrap_or(&text))
        .map(Some)
        .ok_or_else(|| malformed(DIGEST_FILE, "holds no seed digest"))
}

/// What the minter signs: the seed, its node id (a length byte, then its bytes), the mint time
/// and the fleet's mark (16 hex digits).
fn body(seed: &Seed, minter: NodeId, minted_ms: u64, mark: FleetMark) -> Vec<u8> {
    let node = minter.as_bytes();
    let mark = mark.hex();
    let mut body = Vec::with_capacity(16 + 1 + node.len() + 8 + mark.len());
    body.extend_from_slice(seed);
    body.push(u8::try_from(node.len()).unwrap_or(0));
    body.extend_from_slice(node);
    body.extend_from_slice(&minted_ms.to_be_bytes());
    body.extend_from_slice(mark.as_bytes());
    body
}

/// [`CONFLICT_FILE`]'s text for `refused`, refused at `at_ms`:
/// `digest=<16 hex> seed_by=<node> seed_ms=<ms> at_ms=<ms>` and a newline.
#[must_use]
pub fn conflict_line(refused: &FleetSeed, at_ms: u64) -> String {
    format!(
        "{} seed_by={} seed_ms={} at_ms={at_ms}\n",
        conflict_head(refused),
        node_word(refused.minter),
        refused.minted_ms,
    )
}

/// Whether `dir`'s [`CONFLICT_FILE`] records `refused` already, so a seed refused at every
/// round is written once.
#[must_use]
pub fn conflict_recorded(dir: &Path, refused: &FleetSeed) -> bool {
    read_text(dir, CONFLICT_FILE)
        .ok()
        .flatten()
        .is_some_and(|text| text.starts_with(&format!("{} ", conflict_head(refused))))
}

fn conflict_head(refused: &FleetSeed) -> String {
    format!(
        "digest={}",
        aterm_codec::hex::encode_lower(&refused.digest())
    )
}

/// When [`UNHELD_FILE`] says this root first found it held no seed, `None` when it records none
/// or is no time.
#[must_use]
pub fn read_unheld(dir: &Path) -> Option<u64> {
    let text = read_text(dir, UNHELD_FILE).ok()??;
    let ms = text.strip_suffix('\n').unwrap_or(&text);
    (!ms.is_empty() && ms.len() <= 20 && ms.bytes().all(|b| b.is_ascii_digit()))
        .then(|| ms.parse().ok())
        .flatten()
}

/// [`DIGEST_FILE`]'s text for `digest`: 16 lowercase hex digits and a newline.
#[must_use]
pub fn digest_line(digest: [u8; 8]) -> String {
    format!("{}\n", aterm_codec::hex::encode_lower(&digest))
}

fn lower_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    aterm_codec::hex::is_lower(text, 2 * N)
        .then(|| crate::hex_decode(text)?.try_into().ok())
        .flatten()
}

fn read_text(dir: &Path, name: &str) -> io::Result<Option<String>> {
    match super::read_private(dir, name, LINE_MAX) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| malformed(name, "is not UTF-8")),
        Err(super::PrivateFileError::Missing) => Ok(None),
        Err(e) => Err(io::Error::other(format!("{name}: {e}"))),
    }
}

fn malformed(name: &str, what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("{name} {what}"))
}

#[cfg(test)]
mod tests;
