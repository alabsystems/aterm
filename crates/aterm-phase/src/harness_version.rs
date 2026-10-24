// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unified incremental numeric versioning for every CLI harness (`codex-alab`, `claude`, `agy`,
//! `grok`, `kimi`, `muse`, `gemini`).

use std::cmp::Ordering;
use std::fmt;

/// Structured incremental numeric version (`major.minor`) of a CLI harness, at least `0.1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HarnessVersion {
    major: u16,
    minor: u16,
    text: &'static str,
}

impl HarnessVersion {
    /// Initial harness version (`0.1`) shared as the starting point for every CLI harness.
    pub const INITIAL: Self = Self::new(0, 1, "0.1");

    /// Construct a [`HarnessVersion`] whose `text` is the exact canonical
    /// `"<major>.<minor>"` decimal representation and whose version is at least `0.1`.
    ///
    /// # Panics
    /// Panics if `text` is noncanonical, its parsed components differ from `major`
    /// and `minor`, or both components are zero.
    #[inline]
    #[must_use]
    pub const fn new(major: u16, minor: u16, text: &'static str) -> Self {
        match Self::parse_major_minor(text) {
            Some((parsed_major, parsed_minor)) => {
                assert!(
                    text.as_bytes()[0] != b'v',
                    "HarnessVersion text must be canonical"
                );
                assert!(parsed_major == major && parsed_minor == minor);
            }
            None => panic!("invalid HarnessVersion text: expected canonical <major>.<minor>"),
        }
        assert!(major > 0 || minor > 0, "HarnessVersion must be >= 0.1");
        Self { major, minor, text }
    }

    /// Numeric major version component (`0` in `0.1`).
    #[inline]
    #[must_use]
    pub const fn major(self) -> u16 {
        self.major
    }

    /// Numeric minor version component (`1` in `0.1`).
    #[inline]
    #[must_use]
    pub const fn minor(self) -> u16 {
        self.minor
    }

    /// Canonical `(major, minor)` numeric tuple.
    #[inline]
    #[must_use]
    pub const fn tuple(self) -> (u16, u16) {
        (self.major, self.minor)
    }

    /// Canonical string slice (`"0.1"`).
    #[inline]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.text
    }

    /// Monotonic numeric step index (`0.1` -> `1`, `1.0` -> `65536`), preserving
    /// numeric ordering across the full range of both `u16` components.
    #[inline]
    #[must_use]
    pub const fn step(self) -> u32 {
        (self.major as u32) * 65_536 + (self.minor as u32)
    }

    /// Next incremental minor version tuple `(major, minor + 1)`, or `None` when
    /// the minor component is already `u16::MAX`.
    #[inline]
    #[must_use]
    pub const fn next_minor_tuple(self) -> Option<(u16, u16)> {
        match self.minor.checked_add(1) {
            Some(minor) => Some((self.major, minor)),
            None => None,
        }
    }

    /// Next incremental major version tuple `(major + 1, 0)`, or `None` when
    /// the major component is already `u16::MAX`.
    #[inline]
    #[must_use]
    pub const fn next_major_tuple(self) -> Option<(u16, u16)> {
        match self.major.checked_add(1) {
            Some(major) => Some((major, 0)),
            None => None,
        }
    }

    /// Whether `self` is the immediate next minor increment after `prev`.
    #[inline]
    #[must_use]
    pub const fn is_next_minor_of(self, prev: Self) -> bool {
        match prev.minor.checked_add(1) {
            Some(minor) => self.major == prev.major && self.minor == minor,
            None => false,
        }
    }

    /// Const numeric comparison of two [`HarnessVersion`] values (`0.10 > 0.2`).
    #[inline]
    #[must_use]
    pub const fn cmp_num(self, other: Self) -> Ordering {
        if self.major < other.major {
            Ordering::Less
        } else if self.major > other.major {
            Ordering::Greater
        } else if self.minor < other.minor {
            Ordering::Less
        } else if self.minor > other.minor {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }

    /// Parse `"<major>.<minor>"` (or `"v<major>.<minor>"`) into `(major, minor)`. Rejects leading
    /// zeros on multi-digit components, empty parts, non-digit characters and values over
    /// `u16::MAX`.
    #[must_use]
    pub const fn parse_major_minor(s: &str) -> Option<(u16, u16)> {
        let bytes = s.as_bytes();
        let start = if !bytes.is_empty() && bytes[0] == b'v' {
            1
        } else {
            0
        };
        if start >= bytes.len() {
            return None;
        }
        let mut dot = start;
        while dot < bytes.len() && bytes[dot] != b'.' {
            dot += 1;
        }
        if dot == start || dot + 1 >= bytes.len() {
            return None;
        }
        let Some(major) = parse_u16_slice(bytes, start, dot) else {
            return None;
        };
        let Some(minor) = parse_u16_slice(bytes, dot + 1, bytes.len()) else {
            return None;
        };
        Some((major, minor))
    }
}

const fn parse_u16_slice(bytes: &[u8], from: usize, to: usize) -> Option<u16> {
    let len = to - from;
    if len == 0 || (len > 1 && bytes[from] == b'0') {
        return None;
    }
    let mut acc: u32 = 0;
    let mut i = from;
    while i < to {
        let b = bytes[i];
        if b < b'0' || b > b'9' {
            return None;
        }
        acc = acc * 10 + (b - b'0') as u32;
        if acc > u16::MAX as u32 {
            return None;
        }
        i += 1;
    }
    Some(acc as u16)
}

impl PartialOrd for HarnessVersion {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HarnessVersion {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        self.cmp_num(*other)
    }
}

impl fmt::Display for HarnessVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text)
    }
}

/// Every CLI harness with an independent incremental numeric [`HarnessVersion`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CliHarness {
    /// OpenAI Codex / `codex-alab`.
    CodexAlab,
    /// Anthropic Claude Code (`claude`).
    ClaudeCode,
    /// Google Antigravity CLI (`agy`).
    Agy,
    /// xAI Grok (`grok`).
    Grok,
    /// Moonshot Kimi Code (`kimi`).
    Kimi,
    /// Meta Muse Code (`muse`).
    Muse,
    /// Google Gemini CLI (`gemini`).
    Gemini,
}

#[inline]
const fn next_cli_harness(h: CliHarness) -> Option<CliHarness> {
    match h {
        CliHarness::CodexAlab => Some(CliHarness::ClaudeCode),
        CliHarness::ClaudeCode => Some(CliHarness::Agy),
        CliHarness::Agy => Some(CliHarness::Grok),
        CliHarness::Grok => Some(CliHarness::Kimi),
        CliHarness::Kimi => Some(CliHarness::Muse),
        CliHarness::Muse => Some(CliHarness::Gemini),
        CliHarness::Gemini => None,
    }
}

const _: () = {
    let mut at = 0;
    let mut next = Some(CliHarness::ALL[0]);
    while let Some(h) = next {
        assert!(
            at < CliHarness::ALL.len() && h as usize == at && CliHarness::ALL[at] as usize == at
        );
        let v = h.version();
        assert!(v.major() > 0 || v.minor() > 0);
        assert!(v.step() >= 1);
        at += 1;
        next = next_cli_harness(h);
    }
    assert!(at == CliHarness::ALL.len());
};

impl CliHarness {
    /// Every CLI harness in declaration order.
    pub const ALL: [CliHarness; 7] = [
        CliHarness::CodexAlab,
        CliHarness::ClaudeCode,
        CliHarness::Agy,
        CliHarness::Grok,
        CliHarness::Kimi,
        CliHarness::Muse,
        CliHarness::Gemini,
    ];

    /// The five primary named CLI harnesses (`codex-alab`, `claude code`, `agy`, `grok`, `kimi`).
    pub const PRIMARY: [CliHarness; 5] = [
        CliHarness::CodexAlab,
        CliHarness::ClaudeCode,
        CliHarness::Agy,
        CliHarness::Grok,
        CliHarness::Kimi,
    ];

    /// Canonical harness identifier (`"codex-alab"`, `"claude"`, `"agy"`, …).
    #[inline]
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            CliHarness::CodexAlab => "codex-alab",
            CliHarness::ClaudeCode => "claude",
            CliHarness::Agy => "agy",
            CliHarness::Grok => "grok",
            CliHarness::Kimi => "kimi",
            CliHarness::Muse => "muse",
            CliHarness::Gemini => "gemini",
        }
    }

    /// Lowercase CLI executable name: `"codex"` for codex-alab, else the [`id`](Self::id).
    #[inline]
    #[must_use]
    pub const fn cli(self) -> &'static str {
        match self {
            CliHarness::CodexAlab => "codex",
            CliHarness::ClaudeCode => "claude",
            CliHarness::Agy => "agy",
            CliHarness::Grok => "grok",
            CliHarness::Kimi => "kimi",
            CliHarness::Muse => "muse",
            CliHarness::Gemini => "gemini",
        }
    }

    /// Display product name (`"Claude Code"`, `"Antigravity CLI"`, …).
    #[inline]
    #[must_use]
    pub const fn product(self) -> &'static str {
        match self {
            CliHarness::CodexAlab => "codex-alab",
            CliHarness::ClaudeCode => "Claude Code",
            CliHarness::Agy => "Antigravity CLI",
            CliHarness::Grok => "Grok",
            CliHarness::Kimi => "Kimi Code",
            CliHarness::Muse => "Muse Code",
            CliHarness::Gemini => "Gemini CLI",
        }
    }

    /// Structured incremental numeric [`HarnessVersion`] of this CLI harness.
    #[inline]
    #[must_use]
    pub const fn version(self) -> HarnessVersion {
        match self {
            CliHarness::CodexAlab => crate::codex::HARNESS_VER,
            CliHarness::ClaudeCode => crate::phase::HARNESS_VER,
            CliHarness::Agy => crate::agy::HARNESS_VER,
            CliHarness::Grok => crate::grok::HARNESS_VER,
            CliHarness::Kimi => crate::kimi::HARNESS_VER,
            CliHarness::Muse => crate::muse::HARNESS_VER,
            CliHarness::Gemini => crate::gemini::HARNESS_VER,
        }
    }

    /// Canonical version string (`"0.1"`) of this CLI harness.
    #[inline]
    #[must_use]
    pub const fn version_str(self) -> &'static str {
        self.version().as_str()
    }

    /// Numeric major version of this CLI harness.
    #[inline]
    #[must_use]
    pub const fn major(self) -> u16 {
        self.version().major()
    }

    /// Numeric minor version of this CLI harness.
    #[inline]
    #[must_use]
    pub const fn minor(self) -> u16 {
        self.version().minor()
    }

    /// Resolve a CLI name, harness id, or product alias to its [`CliHarness`].
    #[must_use]
    pub fn for_name(name: &str) -> Option<Self> {
        let trimmed = crate::text::trim(name);
        let first = trimmed.split_whitespace().next().unwrap_or("");
        let base = first.rsplit(['/', '\\']).next().unwrap_or(first);
        let word = base.trim_start_matches('-').to_ascii_lowercase();
        match word.as_str() {
            "codex-alab" | "codex" | "codex-cli" | "codex-old" => Some(CliHarness::CodexAlab),
            "claude" | "claude-code" => Some(CliHarness::ClaudeCode),
            "agy" | "antigravity" | "antigravity-cli" => Some(CliHarness::Agy),
            "grok" | "grok-build" => Some(CliHarness::Grok),
            "kimi" | "kimi-code" | "kimi-cli" => Some(CliHarness::Kimi),
            "muse" | "muse-code" => Some(CliHarness::Muse),
            "gemini" | "gemini-cli" => Some(CliHarness::Gemini),
            _ => None,
        }
    }
}

/// Resolve a CLI name, harness id, or product alias to its [`CliHarness`].
#[inline]
#[must_use]
pub fn cli_harness_of(name: &str) -> Option<CliHarness> {
    CliHarness::for_name(name)
}

/// Resolve a CLI name, harness id, or product alias to its structured [`HarnessVersion`].
#[inline]
#[must_use]
pub fn harness_version_of(name: &str) -> Option<HarnessVersion> {
    CliHarness::for_name(name).map(CliHarness::version)
}

/// Resolve a CLI name, harness id, or product alias to its canonical version string (`"0.1"`).
#[inline]
#[must_use]
pub fn harness_version_str_of(name: &str) -> Option<&'static str> {
    CliHarness::for_name(name).map(CliHarness::version_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_and_each_cli_harness_has_consistent_incremental_numeric_version() {
        let expected_primary = [
            (CliHarness::CodexAlab, "codex-alab", "codex", (0, 1), "0.1"),
            (CliHarness::ClaudeCode, "claude", "claude", (0, 1), "0.1"),
            (CliHarness::Agy, "agy", "agy", (0, 1), "0.1"),
            (CliHarness::Grok, "grok", "grok", (0, 1), "0.1"),
            (CliHarness::Kimi, "kimi", "kimi", (0, 1), "0.1"),
        ];
        assert_eq!(CliHarness::PRIMARY.len(), expected_primary.len());
        for (i, &(harness, id, cli, tuple, text)) in expected_primary.iter().enumerate() {
            assert_eq!(CliHarness::PRIMARY[i], harness);
            assert_eq!(harness.id(), id);
            assert_eq!(harness.cli(), cli);
            let ver = harness.version();
            assert_eq!(ver, HarnessVersion::INITIAL);
            assert_eq!(ver.tuple(), tuple);
            assert_eq!(ver.major(), tuple.0);
            assert_eq!(ver.minor(), tuple.1);
            assert_eq!(ver.as_str(), text);
            assert_eq!(ver.to_string(), text);
            assert_eq!(ver.step(), 1);
            assert_eq!(ver.next_minor_tuple(), Some((0, 2)));
            assert_eq!(ver.next_major_tuple(), Some((1, 0)));
            assert_eq!(CliHarness::for_name(id), Some(harness));
            assert_eq!(CliHarness::for_name(cli), Some(harness));
            assert_eq!(CliHarness::for_name(harness.product()), Some(harness));
            assert_eq!(harness_version_of(id), Some(ver));
            assert_eq!(harness_version_str_of(id), Some(text));
        }

        for (name, harness) in [
            ("grok build", CliHarness::Grok),
            (" \tGrOk BuIlD\n", CliHarness::Grok),
            ("gemini cli", CliHarness::Gemini),
            ("\nGeMiNi ClI\t ", CliHarness::Gemini),
        ] {
            assert_eq!(CliHarness::for_name(name), Some(harness), "{name:?}");
        }

        for harness in CliHarness::ALL {
            let ver = harness.version();
            assert_eq!(ver.tuple(), (0, 1), "{harness:?}");
            assert_eq!(ver.as_str(), "0.1", "{harness:?}");
            assert_eq!(
                HarnessVersion::parse_major_minor(ver.as_str()),
                Some(ver.tuple()),
                "{harness:?}"
            );
            assert_eq!(CliHarness::for_name(harness.id()), Some(harness));
            assert_eq!(CliHarness::for_name(harness.cli()), Some(harness));
            assert_eq!(CliHarness::for_name(harness.product()), Some(harness));
        }
        assert_eq!(CliHarness::for_name("Grok Build"), Some(CliHarness::Grok));
        assert_eq!(CliHarness::for_name("Gemini CLI"), Some(CliHarness::Gemini));
        assert_eq!(CliHarness::for_name("Kimi Code"), Some(CliHarness::Kimi));
        assert_eq!(
            CliHarness::for_name("codex-cli"),
            Some(CliHarness::CodexAlab)
        );
    }

    #[test]
    fn numeric_version_ordering_and_increment_rules_hold() {
        let v0_1 = HarnessVersion::new(0, 1, "0.1");
        let v0_2 = HarnessVersion::new(0, 2, "0.2");
        let v0_10 = HarnessVersion::new(0, 10, "0.10");
        let v0_1000 = HarnessVersion::new(0, 1_000, "0.1000");
        let v1_0 = HarnessVersion::new(1, 0, "1.0");
        let max_minor = HarnessVersion::new(0, u16::MAX, "0.65535");
        let max_all = HarnessVersion::new(u16::MAX, u16::MAX, "65535.65535");

        assert!(v0_2.is_next_minor_of(v0_1));
        assert!(!v0_10.is_next_minor_of(v0_2));
        assert!(!max_minor.is_next_minor_of(max_minor));
        assert!(v0_1 < v0_2);
        assert!(v0_2 < v0_10, "numeric ordering, not lexicographic");
        assert!(v0_10 < v0_1000);
        assert!(v0_1000 < v1_0);
        assert_eq!(v0_1.step(), 1);
        assert_eq!(v0_2.step(), 2);
        assert_eq!(v0_10.step(), 10);
        assert_eq!(v0_1000.step(), 1_000);
        assert_eq!(v1_0.step(), 65_536);
        assert_eq!(max_all.step(), u32::MAX);
        assert_eq!(max_all.next_minor_tuple(), None);
        assert_eq!(max_all.next_major_tuple(), None);
        assert!(std::panic::catch_unwind(|| HarnessVersion::new(0, 1, "v0.1")).is_err());

        assert_eq!(HarnessVersion::parse_major_minor("0.1"), Some((0, 1)));
        assert_eq!(HarnessVersion::parse_major_minor("v0.1"), Some((0, 1)));
        assert_eq!(HarnessVersion::parse_major_minor("0.10"), Some((0, 10)));
        assert_eq!(HarnessVersion::parse_major_minor("0.01"), None);
        assert_eq!(HarnessVersion::parse_major_minor("00.1"), None);
        assert_eq!(HarnessVersion::parse_major_minor("0.1.0"), None);
        assert_eq!(HarnessVersion::parse_major_minor(""), None);
    }

    #[test]
    fn a_constructor_rejects_a_prefixed_noncanonical_spelling() {
        assert_eq!(HarnessVersion::parse_major_minor("v0.1"), Some((0, 1)));
        assert!(std::panic::catch_unwind(|| HarnessVersion::new(0, 1, "v0.1")).is_err());
    }

    #[test]
    fn numeric_steps_distinguish_versions_across_the_old_decimal_boundary() {
        let minor = HarnessVersion::new(0, 1_000, "0.1000");
        let major = HarnessVersion::new(1, 0, "1.0");
        assert_ne!(minor, major);
        assert_ne!(minor.step(), major.step());
    }

    #[test]
    fn numeric_steps_preserve_order_across_the_full_minor_range() {
        let minor = HarnessVersion::new(0, u16::MAX, "0.65535");
        let major = HarnessVersion::new(1, 0, "1.0");
        assert!(minor < major);
        assert!(minor.step() < major.step());
    }

    #[test]
    fn the_terminal_minor_is_not_its_own_successor() {
        let last = HarnessVersion::new(0, u16::MAX, "0.65535");
        assert!(!last.is_next_minor_of(last));
    }

    #[test]
    fn asking_for_a_minor_after_the_terminal_component_does_not_panic() {
        let last = HarnessVersion::new(0, u16::MAX, "0.65535");
        assert!(std::panic::catch_unwind(|| last.next_minor_tuple()).is_ok());
    }

    #[test]
    fn asking_for_a_major_after_the_terminal_component_does_not_panic() {
        let last = HarnessVersion::new(u16::MAX, 0, "65535.0");
        assert!(std::panic::catch_unwind(|| last.next_major_tuple()).is_ok());
    }

    #[test]
    fn checked_increment_queries_exhaust_components_without_wrapping() {
        const MAX: HarnessVersion = HarnessVersion::new(u16::MAX, u16::MAX, "65535.65535");
        const MINOR: Option<(u16, u16)> = MAX.next_minor_tuple();
        const MAJOR: Option<(u16, u16)> = MAX.next_major_tuple();
        assert_eq!(MINOR, None);
        assert_eq!(MAJOR, None);
        assert_eq!(MAX.step(), u32::MAX);
        let penultimate = HarnessVersion::new(u16::MAX - 1, u16::MAX - 1, "65534.65534");
        assert_eq!(penultimate.next_minor_tuple(), Some((65_534, 65_535)));
        assert_eq!(penultimate.next_major_tuple(), Some((65_535, 0)));
    }

    #[test]
    fn parsing_rejects_malformed_and_out_of_range_components() {
        assert_eq!(
            HarnessVersion::parse_major_minor("65535.65535"),
            Some((u16::MAX, u16::MAX))
        );
        for invalid in [
            "65536.1",
            "1.65536",
            "999999999999999999999.1",
            "1.999999999999999999999",
            " 0.1",
            "0.1 ",
            "+0.1",
            "0.-1",
            "v",
            "0.01",
            "0.1+gsource",
            "0.1-dev.1",
        ] {
            assert_eq!(
                HarnessVersion::parse_major_minor(invalid),
                None,
                "{invalid}"
            );
        }
    }
}
