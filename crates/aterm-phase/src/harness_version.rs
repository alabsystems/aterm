// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unified incremental numeric versioning for every CLI harness (`codex-alab`, `claude`, `agy`,
//! `grok`, `kimi`, `muse`).
//!
//! A harness version is `MAJOR.MINOR`, never a third part. Its minor goes up by one once per
//! public release window in which the harness's [`CliHarness::sources`] changed, with the window's
//! first such change, so a dev build already shows the number its release ships; the major stays
//! `0` until the owner names a `1.0`. `targo --unverified ship check` refuses a release whose
//! harness sources moved under an unchanged version, and `targo --unverified ship harness-bump`
//! writes the bumps.

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

    /// Numeric major version component (`0` in `0.4`).
    #[inline]
    #[must_use]
    pub const fn major(self) -> u16 {
        self.major
    }

    /// Numeric minor version component (`4` in `0.4`).
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

    /// Canonical string slice (`"0.4"`).
    #[inline]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.text
    }

    /// Parse `"<major>.<minor>"` (or `"v<major>.<minor>"`) into `(major, minor)`. Rejects leading
    /// zeros on multi-digit components, empty parts, a third part, non-digit characters and values
    /// over `u16::MAX`.
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
        self.tuple().cmp(&other.tuple())
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
}

/// The harness lane every CLI harness shares: a change under it moves every harness.
pub const SHARED_LANE: &str = "crates/aterm-agent/src/harness/";

impl CliHarness {
    /// Every CLI harness in declaration order.
    pub const ALL: [CliHarness; 6] = [
        CliHarness::CodexAlab,
        CliHarness::ClaudeCode,
        CliHarness::Agy,
        CliHarness::Grok,
        CliHarness::Kimi,
        CliHarness::Muse,
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
        }
    }

    /// The repository file whose `HARNESS_VER` line holds [`Self::version`].
    #[inline]
    #[must_use]
    pub const fn version_file(self) -> &'static str {
        match self {
            CliHarness::CodexAlab => "crates/aterm-phase/src/codex.rs",
            CliHarness::ClaudeCode => "crates/aterm-phase/src/phase.rs",
            CliHarness::Agy => "crates/aterm-phase/src/agy.rs",
            CliHarness::Grok => "crates/aterm-phase/src/grok.rs",
            CliHarness::Kimi => "crates/aterm-phase/src/kimi.rs",
            CliHarness::Muse => "crates/aterm-phase/src/muse.rs",
        }
    }

    /// What this harness is made of, as repository paths (a trailing `/` names a directory): its
    /// screen reader and the lane every harness shares ([`SHARED_LANE`]). A change under any of
    /// them in a release window moves [`Self::version`]'s minor once.
    #[inline]
    #[must_use]
    pub const fn sources(self) -> [&'static str; 2] {
        [self.version_file(), SHARED_LANE]
    }

    /// Canonical version string (`"0.4"`) of this CLI harness.
    #[inline]
    #[must_use]
    pub const fn version_str(self) -> &'static str {
        self.version().as_str()
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
            _ => None,
        }
    }
}

/// The running CLI's version as a harness label shows it: `raw` cut at its first `-` or `+`
/// (`1.4.13-R6996.1` → `1.4.13`, `0.165.0-dev.4+g1a2b3c4d5` → `0.165.0`), without a leading `v`;
/// `None` where what is left is not dot-joined numbers (`2.1.`, `2..1`, `latest`).
#[must_use]
pub fn cli_version_core(raw: &str) -> Option<&str> {
    let raw = raw.trim();
    let raw = raw.strip_prefix('v').unwrap_or(raw);
    let core = raw.split(['-', '+']).next().unwrap_or("");
    core.split('.')
        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        .then_some(core)
}

/// What a CLI version names past its numbers ([`cli_version_core`]): its pre-release part, cut
/// at its build metadata (`1.4.10-R6500.1` → `R6500.1`, `0.164.0-dev.12521+g1a2b3c4d5` →
/// `dev.12521`); `None` where it has none.
#[must_use]
pub fn cli_version_build(raw: &str) -> Option<&str> {
    cli_version_core(raw)?;
    let raw = raw.trim();
    let (_, rest) = raw.split_once('-')?;
    if raw
        .find('+')
        .is_some_and(|plus| plus < raw.len() - rest.len())
    {
        return None;
    }
    let build = rest.split('+').next().unwrap_or("");
    (!build.is_empty()).then_some(build)
}

/// Whether a move from CLI build `from` onto `to` is one a top rule's ⬆️ offers: a later core
/// ([`cli_version_later`]), or the same core with a later build past it ([`cli_build_later`]: a
/// later Muse revision, a later dev count); never an earlier core, an earlier build under the
/// same core, the same build, or a side that is not a version. It allocates nothing.
#[must_use]
pub fn cli_version_offered(to: &str, from: &str) -> bool {
    if cli_version_core(to).is_none() || cli_version_core(from).is_none() {
        return false;
    }
    cli_version_later(to, from) || (!cli_version_later(from, to) && cli_build_later(to, from))
}

/// Whether CLI version `to`'s build past its core ([`cli_version_build`]) is later than
/// `from`'s: the two are read as runs of digits and runs of other characters and compared run
/// by run, digits as numbers and the others alike or not ordered at all, a build that goes on
/// past the other's end the later (`R6500.1` after `R6427.1`, `R6427.1` after `R6427`,
/// `dev.12521` after `dev.12365`); `false` for builds of other shapes (`R6500.1` against
/// `dev.3`) and where either side has none. It allocates nothing.
#[must_use]
pub fn cli_build_later(to: &str, from: &str) -> bool {
    let (Some(to), Some(from)) = (cli_version_build(to), cli_version_build(from)) else {
        return false;
    };
    let (mut a, mut b) = (Runs(to.as_bytes()), Runs(from.as_bytes()));
    let mut order = Ordering::Equal;
    loop {
        let (x, y) = match (a.next(), b.next()) {
            (None, _) => return order == Ordering::Greater,
            (Some(_), None) => return order != Ordering::Less,
            (Some(x), Some(y)) => (x, y),
        };
        match (x[0].is_ascii_digit(), y[0].is_ascii_digit()) {
            (true, true) => {
                // A run of digits as a number of any size: its digits past the leading zeros,
                // ordered by their count, then as text.
                fn number(run: &[u8]) -> (usize, &[u8]) {
                    let lead = run.iter().take_while(|&&d| d == b'0').count();
                    let rest = &run[lead..];
                    (rest.len(), rest)
                }
                if order == Ordering::Equal {
                    order = number(x).cmp(&number(y));
                }
            }
            (false, false) if x == y => {}
            _ => return false,
        }
    }
}

/// A build's maximal runs of digits and of other bytes, in order ([`cli_build_later`]).
struct Runs<'a>(&'a [u8]);

impl<'a> Runs<'a> {
    fn next(&mut self) -> Option<&'a [u8]> {
        let first = *self.0.first()?;
        let digit = first.is_ascii_digit();
        let len = self
            .0
            .iter()
            .take_while(|b| b.is_ascii_digit() == digit)
            .count();
        let (run, rest) = self.0.split_at(len);
        self.0 = rest;
        Some(run)
    }
}

/// Whether CLI version `to` is numerically later than `from` (their [`cli_version_core`]s
/// compared part by part, a missing part reading 0); `false` where either is not a version.
/// It allocates nothing: a top rule asks it on every frame that draws one.
#[must_use]
pub fn cli_version_later(to: &str, from: &str) -> bool {
    let (Some(to), Some(from)) = (cli_version_core(to), cli_version_core(from)) else {
        return false;
    };
    let number = |part: Option<&str>| part.map_or(Some(0), |p| p.parse::<u64>().ok());
    let (mut a, mut b) = (to.split('.'), from.split('.'));
    loop {
        let (x, y) = (a.next(), b.next());
        if x.is_none() && y.is_none() {
            return false;
        }
        let (Some(x), Some(y)) = (number(x), number(y)) else {
            return false;
        };
        match x.cmp(&y) {
            Ordering::Equal => {}
            later => return later == Ordering::Greater,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cli_harness_names_itself_and_reads_its_own_version() {
        for harness in CliHarness::ALL {
            let ver = harness.version();
            assert!(ver.tuple() >= (0, 1), "{harness:?}");
            assert_eq!(ver.as_str(), harness.version_str(), "{harness:?}");
            assert_eq!(
                ver.as_str(),
                format!("{}.{}", ver.major(), ver.minor()),
                "{harness:?}: the text is the numbers"
            );
            assert_eq!(
                HarnessVersion::parse_major_minor(ver.as_str()),
                Some(ver.tuple()),
                "{harness:?}"
            );
            assert_eq!(CliHarness::for_name(harness.id()), Some(harness));
            assert_eq!(CliHarness::for_name(harness.cli()), Some(harness));
            assert_eq!(CliHarness::for_name(harness.product()), Some(harness));
            let [reader, lane] = harness.sources();
            assert_eq!(reader, harness.version_file());
            assert_eq!(lane, SHARED_LANE);
        }
        assert_eq!(
            CliHarness::CodexAlab.cli(),
            "codex",
            "never `alab codex-alab harness`"
        );
        for (name, harness) in [
            ("grok build", CliHarness::Grok),
            (" \tGrOk BuIlD\n", CliHarness::Grok),
            ("Kimi Code", CliHarness::Kimi),
            ("codex-cli", CliHarness::CodexAlab),
        ] {
            assert_eq!(CliHarness::for_name(name), Some(harness), "{name:?}");
        }
        for retired in ["gemini", "gemini-cli", "Gemini CLI"] {
            assert_eq!(
                CliHarness::for_name(retired),
                None,
                "{retired:?}: agy replaces it"
            );
        }
    }

    #[test]
    fn each_version_file_holds_its_one_line() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for harness in CliHarness::ALL {
            let text = std::fs::read_to_string(dir.join(harness.version_file()))
                .unwrap_or_else(|e| panic!("{harness:?}: {e}"));
            let line = format!(
                concat!(
                    "pub const HARNESS_VER: crate::HarnessVersion = ",
                    "crate::HarnessVersion::new({}, {}, \"{}\");"
                ),
                harness.version().major(),
                harness.version().minor(),
                harness.version_str()
            );
            assert_eq!(text.matches(&line).count(), 1, "{harness:?}: {line}");
        }
    }

    #[test]
    fn numeric_version_ordering_holds() {
        let v0_1 = HarnessVersion::new(0, 1, "0.1");
        let v0_2 = HarnessVersion::new(0, 2, "0.2");
        let v0_10 = HarnessVersion::new(0, 10, "0.10");
        let v0_1000 = HarnessVersion::new(0, 1_000, "0.1000");
        let v1_0 = HarnessVersion::new(1, 0, "1.0");
        let max_minor = HarnessVersion::new(0, u16::MAX, "0.65535");
        assert!(v0_1 < v0_2);
        assert!(v0_2 < v0_10, "numeric ordering, not lexicographic");
        assert!(v0_10 < v0_1000);
        assert!(v0_1000 < v1_0);
        assert!(max_minor < v1_0);
        assert!(std::panic::catch_unwind(|| HarnessVersion::new(0, 1, "v0.1")).is_err());
        assert!(std::panic::catch_unwind(|| HarnessVersion::new(0, 0, "0.0")).is_err());
        assert!(std::panic::catch_unwind(|| HarnessVersion::new(0, 2, "0.1")).is_err());
    }

    #[test]
    fn parsing_takes_two_parts_and_never_a_third() {
        assert_eq!(HarnessVersion::parse_major_minor("0.1"), Some((0, 1)));
        assert_eq!(HarnessVersion::parse_major_minor("v0.1"), Some((0, 1)));
        assert_eq!(HarnessVersion::parse_major_minor("0.10"), Some((0, 10)));
        assert_eq!(
            HarnessVersion::parse_major_minor("65535.65535"),
            Some((u16::MAX, u16::MAX))
        );
        for invalid in [
            "0.1.0",
            "0.4.1",
            "0.01",
            "00.1",
            "",
            "65536.1",
            "1.65536",
            "999999999999999999999.1",
            "1.999999999999999999999",
            " 0.1",
            "0.1 ",
            "+0.1",
            "0.-1",
            "v",
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

    #[test]
    fn a_cli_version_is_cut_to_its_numbers() {
        for (raw, core) in [
            ("2.1.305", Some("2.1.305")),
            ("v2.1.310", Some("2.1.310")),
            ("1.4.13-R6996.1", Some("1.4.13")),
            ("0.165.0-dev.4+g1a2b3c4d5", Some("0.165.0")),
            ("0.1.0-dev.3+codex.0.165.0.g1a2b3c4d5", Some("0.1.0")),
            (" 2.1.305 ", Some("2.1.305")),
            ("", None),
            ("?", None),
            ("latest", None),
            ("-1", None),
            ("2.1.", None),
            ("2..1", None),
            (".2.1", None),
            ("2.1.305.", None),
            ("v", None),
        ] {
            assert_eq!(cli_version_core(raw), core, "{raw:?}");
        }
        assert!(
            !cli_version_later("2.1.", "2.1.0") && !cli_version_later("2.1.0", "2.1."),
            "a malformed core is no version on either side"
        );
        assert!(cli_version_later("2.1.310", "2.1.305"));
        assert!(cli_version_later("2.1.310", "2.1.31"), "numeric, not text");
        assert!(cli_version_later("2.2", "2.1.999"));
        assert!(cli_version_later("1.4.14-R7000.1", "1.4.13-R6996.1"));
        assert!(!cli_version_later("2.1.305", "2.1.305"));
        assert!(!cli_version_later("2.1.305", "2.1.310"));
        assert!(!cli_version_later("2.1.305.0", "2.1.305"));
        assert!(!cli_version_later("latest", "2.1.305"));
        assert!(!cli_version_later("2.1.310", ""));
    }

    /// A LATER REVISION OR DEV BUILD UNDER ONE CORE IS OFFERED, NEVER A STEP BACK: a later core,
    /// a later Muse revision and a later dev count under the same core are moves the ⬆️ offers,
    /// with the build past the core named; the same build, an earlier core, an earlier revision
    /// or dev count under the same core, builds of other shapes and a side that is not a version
    /// are not. CONTROL: `cli_version_later` alone refuses the revision.
    #[test]
    fn a_revision_or_dev_build_under_one_core_is_offered_never_a_step_back() {
        for (to, from) in [
            ("2.1.310", "2.1.305"),
            ("1.4.10-R6500.1", "1.4.10-R6427.1"),
            ("1.4.10-R6427.1", "1.4.10-R6427"),
            ("1.4.10-R6427.10", "1.4.10-R6427.9"),
            ("0.164.0-dev.12521+gabc", "0.164.0-dev.12365+gdef"),
            (
                "0.1.0-dev.13765+codex.0.164.0.g5d4c3b2a1",
                "0.1.0-dev.13764+codex.0.164.0.g4b3a367a0",
            ),
            ("1.4.11-R6500.1", "1.4.10-R6427.1"),
        ] {
            assert!(cli_version_offered(to, from), "{from} -> {to}");
        }
        for (to, from) in [
            ("2.1.305", "2.1.305"),
            ("v2.1.305", "2.1.305"),
            ("2.1.305", "2.1.310"),
            ("1.4.9-R6999.1", "1.4.10-R6427.1"),
            ("1.4.10-R6427.1", "1.4.10-R6500.1"),
            ("1.4.10-R6427", "1.4.10-R6427.1"),
            ("1.4.10-R06427.1", "1.4.10-R6427.1"),
            ("0.164.0-dev.12365+gdef", "0.164.0-dev.12521+gabc"),
            (
                "0.1.0-dev.13764+codex.0.164.0.g4b3a367a0",
                "0.1.0-dev.13765+codex.0.164.0.g5d4c3b2a1",
            ),
            (
                "0.1.0-dev.13764+codex.0.165.0.g5d4c3b2a1",
                "0.1.0-dev.13764+codex.0.164.0.g4b3a367a0",
            ),
            ("1.4.10-R6500.1", "1.4.10-dev.3"),
            ("1.4.10-R6500.1", "1.4.10"),
            ("latest", "2.1.305"),
            ("2.1.310", ""),
            ("2.1.", "2.1.0"),
        ] {
            assert!(!cli_version_offered(to, from), "{from} -> {to}");
        }
        assert!(
            !cli_version_later("1.4.10-R6500.1", "1.4.10-R6427.1"),
            "CONTROL: the cores alone are equal"
        );
        for (raw, build) in [
            ("1.4.10-R6500.1", Some("R6500.1")),
            ("0.164.0-dev.12521+g1a2b3c4d5", Some("dev.12521")),
            ("0.1.0-dev.3+codex.0.165.0.g1a2", Some("dev.3")),
            ("2.1.305", None),
            ("2.1.305+gabc-1", None),
            ("1.4.10-", None),
            ("latest-1", None),
        ] {
            assert_eq!(cli_version_build(raw), build, "{raw:?}");
        }
    }

    #[test]
    fn a_numeric_build_difference_still_checks_the_remaining_paired_shape() {
        for (to, from) in [
            ("1.4.10-R6500.foo", "1.4.10-R6400.bar"),
            ("1.4.10-R6500.2-extra", "1.4.10-R6400.1-other"),
            ("0.1.0-dev.13765.alpha", "0.1.0-dev.13764.beta"),
        ] {
            assert!(
                !cli_version_offered(to, from),
                "different later shape: {from} -> {to}"
            );
            assert!(
                !cli_version_offered(from, to),
                "different earlier shape: {to} -> {from}"
            );
        }
        for (to, from) in [
            ("1.4.10-R6500.1", "1.4.10-R6427.9"),
            ("1.4.10-R6500", "1.4.10-R6427.9"),
        ] {
            assert!(
                cli_version_offered(to, from),
                "compatible shape: {from} -> {to}"
            );
            assert!(!cli_version_offered(from, to), "CONTROL: reverse ordering");
        }
    }
}
