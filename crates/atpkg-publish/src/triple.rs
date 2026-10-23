// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! [`Triple`]: a target the client ships on, and nothing else.
//!
//! A `Triple` is an index into `atpkg::manifest::TARGETS`, so the producer cannot name a
//! target the client does not know; a named constant ([`Triple::named`]) is checked at
//! compile time.

use std::fmt;

use atpkg::manifest::TARGETS;

use crate::error::{Error, Result};

/// One of the client's target triples (`atpkg::manifest::TARGETS`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Triple(usize);

/// `aarch64-apple-darwin`.
pub const AARCH64_APPLE_DARWIN: Triple = Triple::named("aarch64-apple-darwin");
/// `x86_64-apple-darwin`.
pub const X86_64_APPLE_DARWIN: Triple = Triple::named("x86_64-apple-darwin");
/// `x86_64-unknown-linux-gnu`.
pub const X86_64_UNKNOWN_LINUX_GNU: Triple = Triple::named("x86_64-unknown-linux-gnu");
/// `aarch64-unknown-linux-gnu`.
pub const AARCH64_UNKNOWN_LINUX_GNU: Triple = Triple::named("aarch64-unknown-linux-gnu");

/// The operating system a triple runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    /// `*-apple-darwin`.
    Darwin,
    /// `*-linux-*`.
    Linux,
    /// `*-windows-*`.
    Windows,
}

/// The architecture a triple runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    /// `x86_64-*`.
    X86_64,
    /// `aarch64-*`.
    Aarch64,
}

impl Triple {
    /// The client target spelled `name`; in a `const`, a name the client's `TARGETS` lacks
    /// fails the build.
    ///
    /// # Panics
    ///
    /// When `name` is not in `atpkg::manifest::TARGETS` (at compile time in a `const`).
    #[must_use]
    pub const fn named(name: &str) -> Triple {
        let mut i = 0;
        while i < TARGETS.len() {
            if const_str_eq(TARGETS[i], name) {
                return Triple(i);
            }
            i += 1;
        }
        panic!("not a client target triple (atpkg::manifest::TARGETS)");
    }

    /// The client target spelled `s`.
    ///
    /// # Errors
    ///
    /// A usage error naming the client's targets when `s` is not one of them.
    pub fn parse(s: &str) -> Result<Triple> {
        TARGETS
            .iter()
            .position(|t| *t == s)
            .map(Triple)
            .ok_or_else(|| {
                Error::usage(format!(
                    "`{s}` is not a target the client ships on (one of: {})",
                    TARGETS.join(", ")
                ))
            })
    }

    /// Every client target, in the client's own order.
    pub fn all() -> impl Iterator<Item = Triple> {
        (0..TARGETS.len()).map(Triple)
    }

    /// The spelling a manifest row's `target` uses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        TARGETS[self.0]
    }

    /// Its operating system.
    #[must_use]
    pub fn os(self) -> Os {
        let s = self.as_str();
        if s.ends_with("-apple-darwin") {
            Os::Darwin
        } else if s.contains("-linux-") {
            Os::Linux
        } else {
            Os::Windows
        }
    }

    /// Its architecture.
    #[must_use]
    pub fn arch(self) -> Arch {
        if self.as_str().starts_with("aarch64-") {
            Arch::Aarch64
        } else {
            Arch::X86_64
        }
    }

    /// The target this binary was built for — the host it runs on natively. `None` on a
    /// host the client ships no target for.
    #[must_use]
    pub fn host() -> Option<Triple> {
        let name = match (std::env::consts::ARCH, std::env::consts::OS) {
            ("aarch64", "macos") => "aarch64-apple-darwin",
            ("x86_64", "macos") => "x86_64-apple-darwin",
            ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
            ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
            ("x86_64", "windows") => "x86_64-pc-windows-msvc",
            ("aarch64", "windows") => "aarch64-pc-windows-msvc",
            _ => return None,
        };
        Triple::parse(name).ok()
    }
}

impl fmt::Display for Triple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for Triple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

const fn const_str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}
