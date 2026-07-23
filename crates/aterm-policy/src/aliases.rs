// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Named selector aliases (§3.4).
//!
//! These aliases are the human-readable names used in the TOML policy. The
//! alias compiler (#7992) translates them to concrete
//! `SequenceSelector` tokens. Phase 0 ships only the table + lookup so rule
//! authors can reference them; compilation happens in the engine crate.

/// One row of the alias table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AliasEntry {
    /// The alias token that appears in a `Rule::sequence` string.
    pub alias: &'static str,
    /// Human-readable expansion — what the alias stands for. Kept as a doc
    /// comment on the TOML schema; the compiled form is derived inside the
    /// engine crate from the concrete alias name.
    pub expansion: &'static str,
}

/// The full alias table from design §3.4.
///
/// Order is preserved for determinism (tests assert each row by index).
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
        expansion: "OSC 21;<name>=<value> where name in {foreground, background, cursor, selection_background}",
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

/// Number of aliases in the table. Cheap const for tests / FFI advertising.
#[must_use]
pub const fn count() -> usize {
    ALIAS_TABLE.len()
}
