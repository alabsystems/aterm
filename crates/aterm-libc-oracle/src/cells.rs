// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ABI declaration cell table and sibling comparisons ([`crate::siblings`]).

/// The operating-system family of a declaration cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
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
    /// The cell's key in `libc-oracle/union.json` and its conformance module.
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

/// The same-OS pairs whose hand-written body sections must be identical.
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
