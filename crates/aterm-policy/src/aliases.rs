// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Named selector aliases (§3.4).
//!
//! Human-readable names usable in a rule's `sequence`; the selector parser
//! translates them to concrete selector syntax.

/// One row of the alias table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AliasEntry {
    /// The alias token that appears in a `Rule::sequence` string.
    pub alias: &'static str,
    /// What the alias stands for. The compiled form lives in the selector
    /// parser.
    pub expansion: &'static str,
}

/// The full alias table from design §3.4.
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

/// Look up an alias by name. Case-sensitive (TOML authors must match exactly).
#[must_use]
pub fn lookup(alias: &str) -> Option<&'static AliasEntry> {
    ALIAS_TABLE.iter().find(|e| e.alias == alias)
}

/// Number of aliases in the table. Test-only.
#[cfg(test)]
#[must_use]
pub(crate) const fn count() -> usize {
    ALIAS_TABLE.len()
}
