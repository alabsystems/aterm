// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The disk index's model (docs/DESIGN-disk-map-2026-11-18.md §3.2, §6.1).
//!
//! One set of rows feeds the strip, the page, the CLI, the API and the cleaner. This
//! crate holds what a row is and the rules every set of rows obeys; it reads no disk.
//! The walker that fills an [`Index`] is DC-33.
//!
//! - [`NodeId`]: ids from (volume, inode), which survive a rename and a restart.
//! - [`fold`]: the fold rule (64 MiB and 0.5% of the parent) and the 14,000-node bound.
//! - [`Tally`]: bytes by kind of file and by age.
//! - [`Class`], [`Fate`], [`Proof`]: the closed word lists.
//! - [`classify`]: the markers that prove build output, and the class a node's facts give.
//! - [`laws`]: the four laws, each a checked function.

#![warn(missing_docs)]

pub mod classify;
pub mod fold;
pub mod laws;
mod words;

use std::collections::BTreeMap;

pub use words::{Class, Fate, FileKind, Measured, Part, Proof, Region};

/// About 14,000 nodes at most; past that the smallest sibling folds into `rest` (§3.2).
pub const NODE_BOUND: usize = 14_000;

/// A volume, as the filesystem numbers it (`st_dev`): an id is scoped by it.
pub type Volume = u64;

/// A directory on a volume: the pair a rename and a restart leave alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DirKey {
    /// The volume the directory is on.
    pub volume: Volume,
    /// The directory's inode on that volume.
    pub inode: u64,
}

/// A node's id (§3.2's table). Two nodes on different volumes never share one, though
/// their printed forms may agree: the printed form is read within one volume's index.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeId {
    /// A region: `sys`, `home`, `git`, `tmp` or `free`.
    Region(Region),
    /// A repository: `g` and the inode of its git directory.
    Repository(DirKey),
    /// A checkout, build, cache, temp or folder: `d` and its inode.
    Dir(DirKey),
    /// A named part of a checkout: `<checkout id>:<part>`.
    Part(DirKey, Part),
    /// Everything under a parent too small to be a node: `<parent id>:rest`.
    Rest(Box<NodeId>),
}

/// Lowercase Crockford base 32 (no `i`, `l`, `o`, `u`), most significant digit first.
const BASE32: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// `n` in base 32, with no leading zeros (`0` for zero).
#[must_use]
pub fn base32(mut n: u64) -> String {
    let mut digits = Vec::with_capacity(13);
    loop {
        digits.push(BASE32[(n % 32) as usize]);
        n /= 32;
        if n == 0 {
            break;
        }
    }
    digits.reverse();
    String::from_utf8(digits).unwrap_or_default()
}

/// The number a [`base32`] string names; `None` for an empty, foreign or overflowing one.
#[must_use]
pub fn parse_base32(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    s.bytes().try_fold(0u64, |acc, b| {
        let d = BASE32.iter().position(|&c| c == b)? as u64;
        acc.checked_mul(32)?.checked_add(d)
    })
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Region(r) => f.write_str(r.word()),
            Self::Repository(k) => write!(f, "g{}", base32(k.inode)),
            Self::Dir(k) => write!(f, "d{}", base32(k.inode)),
            Self::Part(k, p) => write!(f, "d{}:{}", base32(k.inode), p.word()),
            Self::Rest(parent) => write!(f, "{parent}:rest"),
        }
    }
}

impl NodeId {
    /// Reads a printed id back, on `volume`: the inverse of `Display` within one index.
    #[must_use]
    pub fn parse(volume: Volume, s: &str) -> Option<Self> {
        if let Some(parent) = s.strip_suffix(":rest") {
            return Some(Self::Rest(Box::new(Self::parse(volume, parent)?)));
        }
        if let Some(r) = Region::from_word(s) {
            return Some(Self::Region(r));
        }
        let key = |digits: &str| parse_base32(digits).map(|inode| DirKey { volume, inode });
        if let Some(rest) = s.strip_prefix('g') {
            return key(rest).map(Self::Repository);
        }
        let rest = s.strip_prefix('d')?;
        match rest.split_once(':') {
            Some((digits, part)) => Some(Self::Part(key(digits)?, Part::from_word(part)?)),
            None => key(rest).map(Self::Dir),
        }
    }
}

/// What a node is (§3.2's table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    /// One of the five regions.
    Region,
    /// A git directory.
    Repository,
    /// A main checkout (`linked == false`) or a linked worktree in git's registry.
    Checkout {
        /// Whether git's registry names it as a linked worktree.
        linked: bool,
        /// Whether its tracked and untracked files are pinned in the repository's store
        /// and read back (§6.3); a main checkout's never are.
        pinned: bool,
    },
    /// A named part of a checkout.
    Part,
    /// Build output a marker or a known layout proves.
    Build,
    /// A tool cache.
    Cache,
    /// A temp root or a folder under one.
    Temp,
    /// Any other directory of 64 MiB or more that is at least 0.5% of its parent.
    Folder,
    /// Everything smaller under one parent.
    Rest,
}

/// Bytes by kind of file and by age: `[kind][0]` written today, `[kind][1]` older.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally(pub [[u64; 2]; FileKind::ALL.len()]);

impl Tally {
    /// A tally of `bytes` of one kind, all of one age.
    #[must_use]
    pub fn of(kind: FileKind, today: bool, bytes: u64) -> Self {
        let mut t = Self::default();
        t.0[kind as usize][usize::from(!today)] = bytes;
        t
    }

    /// Every byte the tally holds, saturating at `u64::MAX`.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.0.iter().flatten().fold(0, |s, b| s.saturating_add(*b))
    }

    /// Every byte the tally holds, or `None` when the sum overflows (law 3 reads this).
    #[must_use]
    pub fn checked_total(&self) -> Option<u64> {
        self.0
            .iter()
            .flatten()
            .try_fold(0u64, |s, b| s.checked_add(*b))
    }

    /// The bytes of one kind, of both ages.
    #[must_use]
    pub fn kind(&self, kind: FileKind) -> u64 {
        let [today, older] = self.0[kind as usize];
        today.saturating_add(older)
    }

    /// The bytes written today, of every kind.
    #[must_use]
    pub fn today(&self) -> u64 {
        self.0.iter().fold(0, |s, ages| s.saturating_add(ages[0]))
    }

    /// Adds `other` into this tally, saturating: an overflowed figure no longer sums
    /// to its parts, and law 1 or 3 refuses it.
    pub fn add(&mut self, other: &Self) {
        for (mine, theirs) in self.0.iter_mut().zip(other.0.iter()) {
            mine[0] = mine[0].saturating_add(theirs[0]);
            mine[1] = mine[1].saturating_add(theirs[1]);
        }
    }

    /// The bytes of a person's text, documents and media (law 4's first rank).
    #[must_use]
    pub fn personal(&self) -> u64 {
        self.kind(FileKind::Documents)
            .saturating_add(self.kind(FileKind::Media))
    }
}

/// Why a node has its class: the proof words that hold (a closed list) and the facts
/// read on the way, in words a row prints.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evidence {
    /// The proofs that the bytes are temporary or held elsewhere.
    pub proofs: Vec<Proof>,
    /// What was read: `git-ignored`, `Cargo's marker`, `a bare CACHEDIR.TAG proves nothing`…
    pub facts: Vec<String>,
}

/// One row of the index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// Its id.
    pub id: NodeId,
    /// What it is.
    pub kind: NodeKind,
    /// The parent's id; `None` only for a region.
    pub parent: Option<NodeId>,
    /// A name to draw (the folder's last component, a part's word).
    pub name: String,
    /// Allocated bytes, a hard link counted once.
    pub bytes: u64,
    /// Bytes by kind and age.
    pub tally: Tally,
    /// Bytes by class: law 3 holds them to `bytes`.
    pub classes: BTreeMap<Class, u64>,
    /// Why.
    pub evidence: Evidence,
    /// What the cleaner does with it.
    pub fate: Fate,
    /// Under a temp root (rule 3's exception).
    pub under_temp_root: bool,
    /// The newest write inside, seconds since the epoch.
    pub newest_write: Option<u64>,
    /// Growth over the last minute and the last hour, signed bytes.
    pub growth: [i64; 2],
    /// When it was measured, seconds since the epoch.
    pub as_of: u64,
    /// Measured by the survey, or carried from the holders ledger (`measured=carried`).
    pub measured: Measured,
}

impl Node {
    /// The class with the most bytes: the one word a cell is drawn by.
    #[must_use]
    pub fn class(&self) -> Class {
        self.classes
            .iter()
            .max_by_key(|&(class, bytes)| (*bytes, std::cmp::Reverse(*class)))
            .map_or(Class::Unread, |(class, _)| *class)
    }

    /// A leaf of one class: its class figures are its bytes.
    #[must_use]
    pub fn leaf(
        id: NodeId,
        kind: NodeKind,
        parent: NodeId,
        name: &str,
        class: Class,
        tally: Tally,
    ) -> Self {
        let bytes = tally.total();
        Self {
            id,
            kind,
            parent: Some(parent),
            name: name.to_owned(),
            bytes,
            tally,
            classes: BTreeMap::from([(class, bytes)]),
            evidence: Evidence::default(),
            fate: Fate::None,
            under_temp_root: false,
            newest_write: None,
            growth: [0, 0],
            as_of: 0,
            measured: Measured::Yes,
        }
    }
}

/// One volume's index: its rows and the figures the laws hold them to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Index {
    /// The volume.
    pub volume: Volume,
    /// The volume's used bytes, from the filesystem.
    pub used: u64,
    /// Used bytes outside every node, found by subtraction: never called kept.
    pub unreached: u64,
    /// The rows, by id.
    pub nodes: BTreeMap<NodeId, Node>,
}

/// The published numbers for bytes the index cannot class (§3.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Unclassed {
    /// Inside a node the survey has not reached yet.
    pub unread: u64,
    /// Read, and no rule says what it is.
    pub unexplained: u64,
    /// Used bytes outside every node.
    pub unreached: u64,
    /// Git-ignored bytes in a checkout that no marker proves (DC-D19: the AI's question
    /// opens at 1 GiB of them). They are part of `unexplained` as well.
    pub unproven_ignored: u64,
}

impl Index {
    /// Adds a row, replacing one with the same id.
    pub fn insert(&mut self, node: Node) {
        self.nodes.insert(node.id.clone(), node);
    }

    /// Each node's children, by parent id.
    #[must_use]
    pub fn children(&self) -> BTreeMap<&NodeId, Vec<&NodeId>> {
        let mut map: BTreeMap<&NodeId, Vec<&NodeId>> = BTreeMap::new();
        for node in self.nodes.values() {
            if let Some(parent) = &node.parent {
                map.entry(parent).or_default().push(&node.id);
            }
        }
        map
    }

    /// The node's ancestors, nearest first; stops at a missing parent or a cycle.
    #[must_use]
    pub fn ancestors(&self, id: &NodeId) -> Vec<&Node> {
        let mut out: Vec<&Node> = Vec::new();
        let mut at = self.nodes.get(id).and_then(|n| n.parent.as_ref());
        while let Some(parent) = at.and_then(|p| self.nodes.get(p)) {
            if out.iter().any(|n| n.id == parent.id) || parent.id == *id {
                break;
            }
            out.push(parent);
            at = parent.parent.as_ref();
        }
        out
    }

    /// Sets every parent's bytes, tally and class figures to its children's sums,
    /// deepest first, so laws 1 and 3 hold of what the leaves say. A sum that overflows
    /// saturates, and law 1 then refuses it.
    pub fn roll_up(&mut self) {
        let depth = |index: &Self, id: &NodeId| index.ancestors(id).len();
        let mut parents: Vec<(usize, NodeId)> = self
            .children()
            .keys()
            .map(|id| (depth(self, id), (*id).clone()))
            .collect();
        parents.sort_by_key(|p| std::cmp::Reverse(p.0));
        for (_, parent) in parents {
            let (mut bytes, mut tally, mut classes) =
                (0u64, Tally::default(), BTreeMap::<Class, u64>::new());
            for child in self
                .nodes
                .values()
                .filter(|n| n.parent.as_ref() == Some(&parent))
            {
                bytes = bytes.saturating_add(child.bytes);
                tally.add(&child.tally);
                for (class, b) in &child.classes {
                    let figure = classes.entry(*class).or_default();
                    *figure = figure.saturating_add(*b);
                }
            }
            if let Some(node) = self.nodes.get_mut(&parent) {
                node.bytes = bytes;
                node.tally = tally;
                node.classes = classes;
            }
        }
    }

    /// The published numbers: unread and unexplained summed over the regions' class
    /// figures, unreached as the index holds it, and the unexplained bytes of every
    /// `ignored/<prefix>` part.
    #[must_use]
    pub fn unclassed(&self) -> Unclassed {
        let sum = |class| {
            self.nodes
                .values()
                .filter(|n| n.parent.is_none())
                .map(|n| n.classes.get(&class).copied().unwrap_or(0))
                .fold(0u64, u64::saturating_add)
        };
        let unproven_ignored = self
            .nodes
            .values()
            .filter(|n| matches!(n.id, NodeId::Part(_, Part::Ignored(_))))
            .map(|n| n.classes.get(&Class::Unexplained).copied().unwrap_or(0))
            .fold(0u64, u64::saturating_add);
        Unclassed {
            unread: sum(Class::Unread),
            unexplained: sum(Class::Unexplained),
            unreached: self.unreached,
            unproven_ignored,
        }
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
