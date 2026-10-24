// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Named selector aliases usable in `Rule::sequence`.

/// Named selector alias entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AliasEntry {
    /// Alias token usable in `Rule::sequence`.
    pub alias: &'static str,
    /// Human-readable description of the selector expansion.
    pub expansion: &'static str,
}

/// Built-in selector alias table.
pub const ALIAS_TABLE: &[AliasEntry] = &[
    AliasEntry {
        alias: "OSC 52 set",
        expansion: "OSC 52;<selection>;<base64_non_question>",
    },
    AliasEntry {
        alias: "OSC 52 query",
        expansion: "OSC 52;<selection>;?",
    },
    AliasEntry {
        alias: "OSC 4 query",
        expansion: "OSC 4;<idx>;?",
    },
    AliasEntry {
        alias: "OSC 4 set",
        expansion: "OSC 4;<idx>;<not-question>",
    },
    AliasEntry {
        alias: "OSC 21 set named",
        expansion: "OSC 21;<name>=<value> where name in {foreground, background, cursor, selection_background, selection_foreground}",
    },
    AliasEntry {
        alias: "OSC 21 set indexed",
        expansion: "OSC 21;<idx>=<value>",
    },
    AliasEntry {
        alias: "response any",
        expansion: "catch-all for the 31 response-producing sequences",
    },
];

/// Look up an alias by exact case-sensitive name.
#[must_use]
#[inline]
pub fn lookup(alias: &str) -> Option<&'static AliasEntry> {
    ALIAS_TABLE.iter().find(|e| e.alias == alias)
}
