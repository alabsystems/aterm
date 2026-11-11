// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unified incremental numeric versioning for every CLI harness (`claude`, `agy`,
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

/// The source spelling that begins a harness's compiled version declaration.
pub const SOURCE_DECLARATION_HEAD: &str =
    "pub const HARNESS_VER: crate::HarnessVersion = crate::HarnessVersion::new(";

/// The `(major, minor)` a harness source file declares: either the current one-line shape
/// (`HarnessVersion::new(0, 4, "0.4")`) or the three-constant shape it replaced.
#[must_use]
pub fn declared_in_source(text: &str) -> Option<(u16, u16)> {
    let uint = |s: &str| {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u16>().ok())
            .flatten()
    };
    let at = text.find(SOURCE_DECLARATION_HEAD)?;
    let args = text[at + SOURCE_DECLARATION_HEAD.len()..]
        .split(");")
        .next()?;
    let text_arg = args.split('"').nth(1);
    let mut parts = args.split(',');
    if let (Some(major), Some(minor), Some(spelled)) = (
        parts.next().and_then(|a| uint(a.trim())),
        parts.next().and_then(|a| uint(a.trim())),
        text_arg,
    ) {
        return (HarnessVersion::parse_major_minor(spelled) == Some((major, minor)))
            .then_some((major, minor));
    }
    let constant = |name: &str| -> Option<&str> {
        let key = format!("pub const {name}: ");
        let rest = &text[text.find(&key)? + key.len()..];
        Some(rest.split(';').next()?.split('=').nth(1)?.trim())
    };
    let spelled = constant("HARNESS_VERSION")?.trim_matches('"');
    let parsed = HarnessVersion::parse_major_minor(spelled)?;
    let major = uint(constant("HARNESS_VERSION_MAJOR")?)?;
    let minor = uint(constant("HARNESS_VERSION_MINOR")?)?;
    (parsed == (major, minor)).then_some(parsed)
}

/// The canonical one-line source declaration for `(major, minor)`.
#[must_use]
pub fn source_line((major, minor): (u16, u16)) -> String {
    format!("{SOURCE_DECLARATION_HEAD}{major}, {minor}, \"{major}.{minor}\");")
}

/// `text` with its compiled harness version rewritten to `to` in the one-line shape. The
/// retired three-constant declaration is collapsed when encountered.
#[must_use]
pub fn rewrite_source(text: &str, to: (u16, u16)) -> Option<String> {
    declared_in_source(text)?;
    let start = text.find(SOURCE_DECLARATION_HEAD)?;
    let end = start + text[start..].find(");")? + 2;
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..start]);
    out.push_str(&source_line(to));
    out.push_str(&text[end..]);
    for name in [
        "HARNESS_VERSION_MAJOR",
        "HARNESS_VERSION_MINOR",
        "HARNESS_VERSION",
    ] {
        let key = format!("pub const {name}: ");
        if let Some(at) = out.find(&key) {
            let line_start = out[..at].rfind("\n\n").map_or(0, |p| p + 1);
            let line_end = at + out[at..].find(";\n").map_or(out.len() - at, |p| p + 2);
            out.replace_range(line_start..line_end, "");
        }
    }
    Some(out)
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

impl CliHarness {
    /// Every CLI harness in declaration order.
    pub const ALL: [CliHarness; 5] = [
        CliHarness::ClaudeCode,
        CliHarness::Agy,
        CliHarness::Grok,
        CliHarness::Kimi,
        CliHarness::Muse,
    ];

    /// Canonical harness identifier (`"claude"`, `"agy"`, …).
    #[inline]
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            CliHarness::ClaudeCode => "claude",
            CliHarness::Agy => "agy",
            CliHarness::Grok => "grok",
            CliHarness::Kimi => "kimi",
            CliHarness::Muse => "muse",
        }
    }

    /// Lowercase CLI executable name ([`Self::id`]).
    #[inline]
    #[must_use]
    pub const fn cli(self) -> &'static str {
        match self {
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
            CliHarness::ClaudeCode => "crates/aterm-phase/src/phase.rs",
            CliHarness::Agy => "crates/aterm-phase/src/agy.rs",
            CliHarness::Grok => "crates/aterm-phase/src/grok.rs",
            CliHarness::Kimi => "crates/aterm-phase/src/kimi.rs",
            CliHarness::Muse => "crates/aterm-phase/src/muse.rs",
        }
    }

    /// What this harness is made of, as repository paths (a trailing `/` names a directory): its
    /// screen reader ([`Self::version_file`]) and its own harness files under
    /// `crates/aterm-agent/src/harness/`. Every CLI harness owns a disjoint source set; a change
    /// under any of them in a release window moves [`Self::version`]'s minor once without bumping
    /// any other harness.
    #[inline]
    #[must_use]
    pub const fn sources(self) -> &'static [&'static str] {
        match self {
            CliHarness::ClaudeCode => &[
                "crates/aterm-phase/src/phase.rs",
                "crates/aterm-agent/src/harness/cli_shared_claude.rs",
                "crates/aterm-agent/src/harness/upgrade.rs",
                "crates/aterm-agent/src/harness/upgrade_drive.rs",
            ],
            CliHarness::Agy => &[
                "crates/aterm-phase/src/agy.rs",
                "crates/aterm-agent/src/harness/agy_quota.rs",
                "crates/aterm-agent/src/harness/agy_quota_store.rs",
                "crates/aterm-agent/src/harness/upgrade_agy.rs",
                "crates/aterm-agent/src/harness/upgrade_agy_drive.rs",
                "crates/aterm-agent/src/harness/upgrade_agy_live.rs",
                "crates/aterm-agent/src/harness/upgrade_agy_live_drive.rs",
            ],
            CliHarness::Grok => &[
                "crates/aterm-phase/src/grok.rs",
                "crates/aterm-agent/src/harness/grok_signin.rs",
                "crates/aterm-agent/src/harness/worker_grok.rs",
            ],
            CliHarness::Kimi => &[
                "crates/aterm-phase/src/kimi.rs",
                "crates/aterm-agent/src/harness/cli_shared_kimi.rs",
            ],
            CliHarness::Muse => &[
                "crates/aterm-phase/src/muse.rs",
                "crates/aterm-agent/src/harness/muse_usage.rs",
                "crates/aterm-agent/src/harness/upgrade_muse.rs",
                "crates/aterm-agent/src/harness/upgrade_muse_drive.rs",
                "crates/aterm-agent/src/harness/upgrade_muse_live.rs",
                "crates/aterm-agent/src/harness/upgrade_muse_live_drive.rs",
            ],
        }
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
    fn number(part: Option<&str>) -> (usize, &str) {
        let stripped = part.unwrap_or("").trim_start_matches('0');
        (stripped.len(), stripped)
    }
    let (Some(to), Some(from)) = (cli_version_core(to), cli_version_core(from)) else {
        return false;
    };
    let (mut a, mut b) = (to.split('.'), from.split('.'));
    loop {
        let (x, y) = (a.next(), b.next());
        if x.is_none() && y.is_none() {
            return false;
        }
        match number(x).cmp(&number(y)) {
            Ordering::Equal => {}
            later => return later == Ordering::Greater,
        }
    }
}

#[cfg(test)]
#[path = "harness_version_tests.rs"]
mod tests;
