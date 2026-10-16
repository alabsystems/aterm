// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE cell table: every ABI cell `crates/aterm-libc` declares, once.
//!
//! A cell is decided on a box native to it, under Trust alone (Trust ships std
//! for the host triple only), so a run decides exactly one row of this table.
//! The other rows still matter here: the hand-written body sections are text, and
//! each same-OS pair is compared on every box ([`crate::siblings`]).
//!
//! The Windows and wasm targets are not rows: the shim declares nothing there
//! beyond the `core::ffi` primitives, and `libc-oracle/conformance`'s `cell_zero`
//! pins that surface wherever it is compiled.

use serde::Serialize;

/// The operating-system family of a declaration cell; its hand-written bodies are
/// per family, so the two architectures of one family must carry the same text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    Darwin,
    GnuLinux,
}

impl Os {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Darwin => "Darwin",
            Self::GnuLinux => "GNU Linux",
        }
    }
}

/// One ABI cell of the shim.
#[derive(Debug, PartialEq, Eq)]
pub struct Cell {
    /// The target triple a box must be native to for this cell to be decided.
    pub triple: &'static str,
    /// The cell's key in `libc-oracle/union.json` and its conformance module
    /// (`libc-oracle/conformance/src/cell_<key>.rs`).
    pub key: &'static str,
    pub os: Os,
    /// `target_arch` as the cell's `#[cfg]` spells it.
    pub arch: &'static str,
    /// The cell's module under `crates/aterm-libc/src/`.
    pub source: &'static str,
}

pub const CELLS: [Cell; 4] = [
    Cell {
        triple: "aarch64-apple-darwin",
        key: "mac",
        os: Os::Darwin,
        arch: "aarch64",
        source: "darwin_aarch64.rs",
    },
    Cell {
        triple: "x86_64-apple-darwin",
        key: "macx",
        os: Os::Darwin,
        arch: "x86_64",
        source: "darwin_x86_64.rs",
    },
    Cell {
        triple: "x86_64-unknown-linux-gnu",
        key: "linux",
        os: Os::GnuLinux,
        arch: "x86_64",
        source: "linux_gnu_x86_64.rs",
    },
    Cell {
        triple: "aarch64-unknown-linux-gnu",
        key: "linuxa",
        os: Os::GnuLinux,
        arch: "aarch64",
        source: "linux_gnu_aarch64.rs",
    },
];

/// The cell a box native to `triple` decides, if the shim declares one there.
#[must_use]
pub fn for_host(triple: &str) -> Option<&'static Cell> {
    CELLS.iter().find(|cell| cell.triple == triple)
}

/// The triples [`CELLS`] names, for a diagnostic.
#[must_use]
pub fn triples() -> Vec<&'static str> {
    CELLS.iter().map(|cell| cell.triple).collect()
}

/// The same-OS pairs whose hand-written body sections must be identical: each
/// family's first cell against every later one, in table order.
#[must_use]
pub fn sibling_pairs() -> Vec<(Os, &'static Cell, &'static Cell)> {
    let mut pairs = Vec::new();
    for (i, first) in CELLS.iter().enumerate() {
        if CELLS[..i].iter().any(|earlier| earlier.os == first.os) {
            continue;
        }
        for later in CELLS[i + 1..].iter().filter(|c| c.os == first.os) {
            pairs.push((first.os, first, later));
        }
    }
    pairs
}

impl Cell {
    /// The cell's own `#[cfg]` predicate, as the shim's dispatch spells it.
    #[must_use]
    pub fn cfg(&self) -> String {
        match self.os {
            Os::Darwin => format!(r#"all(target_os = "macos", target_arch = "{}")"#, self.arch),
            Os::GnuLinux => format!(
                r#"all(target_os = "linux", target_env = "gnu", target_arch = "{}")"#,
                self.arch
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_finds_its_cell_and_nothing_else() {
        let cell = for_host("aarch64-apple-darwin").expect("the Apple silicon cell");
        assert_eq!(
            (cell.key, cell.os, cell.source),
            ("mac", Os::Darwin, "darwin_aarch64.rs")
        );
        assert_eq!(
            for_host("x86_64-unknown-linux-gnu").map(|c| c.key),
            Some("linux")
        );
        assert_eq!(for_host("x86_64-pc-windows-msvc"), None);
        assert_eq!(for_host("wasm32-unknown-unknown"), None);
        assert_eq!(for_host(""), None);
    }

    #[test]
    fn each_family_pairs_its_first_cell_with_the_other() {
        let pairs: Vec<_> = sibling_pairs()
            .into_iter()
            .map(|(os, left, right)| (os, left.source, right.source))
            .collect();
        assert_eq!(
            pairs,
            [
                (Os::Darwin, "darwin_aarch64.rs", "darwin_x86_64.rs"),
                (Os::GnuLinux, "linux_gnu_x86_64.rs", "linux_gnu_aarch64.rs"),
            ]
        );
    }

    #[test]
    fn a_cells_cfg_names_its_os_and_arch() {
        assert_eq!(
            CELLS[0].cfg(),
            r#"all(target_os = "macos", target_arch = "aarch64")"#
        );
        assert_eq!(
            CELLS[3].cfg(),
            r#"all(target_os = "linux", target_env = "gnu", target_arch = "aarch64")"#
        );
    }
}
