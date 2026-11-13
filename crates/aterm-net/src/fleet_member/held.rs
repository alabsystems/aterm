// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verdicts of [`Peer::signature_holds`] kept between reads of a book, for a reader that judges
//! the same book again and again. The check is a function of the entry's node id, node key,
//! host, port and version and of its signature ([`Signed::holds`]) and of nothing else, so an
//! entry equal in all of them to one judged before holds or fails as that one did.

use super::{NodeKey, Peer, Signed};
use std::collections::BTreeMap;

/// One judged entry: every field [`Peer::signature_holds`] reads but the node id it is filed
/// under, and what the check came to.
#[derive(Clone, Debug)]
struct Judged {
    key: NodeKey,
    host: String,
    port: u16,
    version: String,
    /// The signature whole: its self-seq, its bytes, the signed primary name, and the name
    /// record it carries (which the check does not read; a changed one only checks again).
    signed: Signed,
    holds: bool,
}

impl Judged {
    fn is(&self, key: &NodeKey, peer: &Peer, version: &str, signed: &Signed) -> bool {
        self.key == *key
            && self.host == peer.host
            && self.port == peer.port
            && self.version == version
            && self.signed == *signed
    }
}

/// The verdicts of the last book judged ([`Self::judge`]), each under everything its check
/// read. It keeps a failed verdict as it keeps one that held, and nothing but the entries'
/// signed fields and those verdicts.
#[derive(Debug, Default)]
pub struct HeldSignatures {
    /// By node id; a node the book names twice has each of its entries.
    kept: BTreeMap<String, Vec<Judged>>,
}

impl HeldSignatures {
    /// Whether each of `book`'s entries' signatures holds ([`Peer::signature_holds`]), in the
    /// book's order. An entry equal, in every field the check reads, to one the last call judged
    /// (or to an earlier one of this book) takes that verdict and is not checked again; any
    /// other is checked now. The verdicts kept afterwards are this book's alone: an entry the
    /// book no longer holds is forgotten.
    #[must_use]
    pub fn judge(&mut self, book: &[Peer]) -> Vec<bool> {
        let mut kept: BTreeMap<String, Vec<Judged>> = BTreeMap::new();
        let verdicts = book
            .iter()
            .map(|peer| {
                // An entry that lacks any of these holds nothing ([`Peer::signature_holds`]).
                let (Some(node), Some(key), Some(version), Some(signed)) =
                    (&peer.node, &peer.key, &peer.version, &peer.signed)
                else {
                    return false;
                };
                let judged = |held: &BTreeMap<String, Vec<Judged>>| {
                    held.get(node)?
                        .iter()
                        .find(|judged| judged.is(key, peer, version, signed))
                        .map(|judged| judged.holds)
                };
                if let Some(holds) = judged(&kept) {
                    return holds;
                }
                let holds = judged(&self.kept).unwrap_or_else(|| peer.signature_holds());
                kept.entry(node.clone()).or_default().push(Judged {
                    key: *key,
                    host: peer.host.clone(),
                    port: peer.port,
                    version: version.clone(),
                    signed: *signed,
                    holds,
                });
                holds
            })
            .collect();
        self.kept = kept;
        verdicts
    }
}

#[cfg(test)]
#[path = "held_tests.rs"]
mod tests;
