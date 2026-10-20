// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! On-demand vendor-file drift acceptance. Run `targo --unverified bench -p
//! aterm-phase --bench vendor_drift`; missing builds are COULD NOT RUN, never
//! a pass. Reads vendor files and atpkg records without starting the vendors.

use anchors::*;
use aterm_phase::anchors;
use aterm_phase::wall::PHRASES;

#[path = "../../aterm-conformance/benches/support/mod.rs"]
mod support;
#[path = "../tests/support/vendor_drift.rs"]
mod vendor_drift;
#[path = "../tests/support/vendor_files.rs"]
mod vendor_files;

use std::process::ExitCode;
use support::Outcome;
use vendor_drift::*;
use vendor_files::*;

type Check = fn() -> Outcome;
const ROWS: &[(&str, Check)] = &[
    (
        "every_literal_anchor_is_in_the_store_claude",
        every_literal_anchor_is_in_the_store_claude,
    ),
    (
        "every_transcript_key_is_in_the_store_claude",
        every_transcript_key_is_in_the_store_claude,
    ),
    (
        "every_rollout_key_is_in_the_store_codex",
        every_rollout_key_is_in_the_store_codex,
    ),
    (
        "every_reset_template_is_whole_in_the_store_claude",
        every_reset_template_is_whole_in_the_store_claude,
    ),
    (
        "every_literal_agy_anchor_is_in_the_store_agy",
        every_literal_agy_anchor_is_in_the_store_agy,
    ),
    (
        "every_literal_muse_anchor_is_in_the_store_muse",
        every_literal_muse_anchor_is_in_the_store_muse,
    ),
    (
        "every_literal_gemini_anchor_is_in_its_bundle",
        every_literal_gemini_anchor_is_in_its_bundle,
    ),
    (
        "every_wall_phrase_is_in_the_store_claude",
        every_wall_phrase_is_in_the_store_claude,
    ),
];

fn main() -> ExitCode {
    support::run("phase_vendor_drift", ROWS, |check| check())
}

/// Whether `bytes` are a build of a program rather than a stub: a Mach-O or
/// ELF header, or at least [`MIN_BUILD_BYTES`]. A reroute stub
/// (`<prefix>/reroute/<program>`, a few lines of shell) answers `aterm pkg
/// which` on a box whose resolved token is the stub itself; it holds none of
/// the anchors, so "missing" would be the stub's, not the build's.
pub(crate) fn looks_like_a_build(bytes: &[u8]) -> bool {
    const MAGICS: [[u8; 4]; 6] = [
        [0xfe, 0xed, 0xfa, 0xce],
        [0xfe, 0xed, 0xfa, 0xcf],
        [0xce, 0xfa, 0xed, 0xfe],
        [0xcf, 0xfa, 0xed, 0xfe],
        [0xca, 0xfe, 0xba, 0xbe],
        [0x7f, b'E', b'L', b'F'],
    ];
    bytes.len() >= MIN_BUILD_BYTES
        || bytes
            .get(..4)
            .is_some_and(|head| MAGICS.iter().any(|magic| magic == head))
}
/// Below this, a file that is no native executable is a stub script.
const MIN_BUILD_BYTES: usize = 4 * 1024 * 1024;

/// [`canary_path`] for `program`: `$<env>` first, else what `aterm pkg which`
/// says (`None` where it says nothing).
pub(crate) fn canary_binary(program: &str, env: &str) -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os(env) {
        return Some(path.into());
    }
    // Through the workspace's one child builder (a dev-dependency: this
    // crate has none), so the child holds its stdio and nothing else.
    let out = aterm_uds::spawnfd::command("aterm")
        .args(["pkg", "which", program])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    canary_path(text.lines().next()?, home.as_deref())
}

/// A real managed vendor build; a reroute stub or unreadable entry cannot
/// decide a drift check. Version records are read separately, never by spawning.
fn store_binary(program: &str, env: &str) -> Option<(String, Vec<u8>)> {
    let path = canary_binary(program, env)?.to_string_lossy().into_owned();
    let bytes = std::fs::read(&path).ok()?;
    if !looks_like_a_build(&bytes) {
        eprintln!("{path} is a stub rather than a vendor build");
        return None;
    }
    Some((path, bytes))
}

/// Which of `needles` occur in `hay`, by ONE pass over it: a needle is
/// compared only where its first three bytes are, and the pass ends once
/// every needle is found. A vendor build is 190–375 MB and a table some two
/// hundred needles, so a [`contains_bytes`] per needle, in the test
/// profile's unoptimised code, took 31 s for Muse Code's table.
pub(crate) fn found_in(hay: &[u8], needles: &[&[u8]]) -> Vec<bool> {
    const HEAD: usize = 3;
    let mut found: Vec<bool> = needles.iter().map(|n| n.is_empty()).collect();
    // `heads[h]`: 1 + the index in `groups` of the needles whose first three
    // bytes, big-endian, are `h`; 0 for none.
    let mut heads = vec![0u16; 1 << (8 * HEAD)];
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut left = 0usize;
    for (i, needle) in needles.iter().enumerate() {
        if needle.is_empty() {
            continue;
        }
        if needle.len() < HEAD {
            found[i] = hay.windows(needle.len()).any(|w| w == *needle);
            continue;
        }
        let h = needle[..HEAD]
            .iter()
            .fold(0, |h, &b| (h << 8) | usize::from(b));
        if heads[h] == 0 {
            groups.push(Vec::new());
            heads[h] = u16::try_from(groups.len()).expect("fewer than 65,536 heads");
        }
        groups[usize::from(heads[h]) - 1].push(i);
        left += 1;
    }
    let mut window = 0usize;
    for (end, &b) in hay.iter().enumerate() {
        if left == 0 {
            break;
        }
        window = ((window << 8) | usize::from(b)) & ((1 << (8 * HEAD)) - 1);
        let group = heads[window];
        if group == 0 || end + 1 < HEAD {
            continue;
        }
        let at = &hay[end + 1 - HEAD..];
        for &i in &groups[usize::from(group) - 1] {
            if !found[i] && at.starts_with(needles[i]) {
                found[i] = true;
                left -= 1;
            }
        }
    }
    found
}

/// The literal anchors of `table` missing from `bytes`: a row whose
/// parts (split at ` · `) are not all in them.
pub(crate) fn missing(bytes: &[u8], table: &[Anchor]) -> Vec<&'static str> {
    let literal: Vec<&Anchor> = table
        .iter()
        .filter(|a| a.kind == AnchorKind::Literal)
        .collect();
    let parts: Vec<&[u8]> = literal
        .iter()
        .flat_map(|a| a.text.split(" · "))
        .map(str::as_bytes)
        .collect();
    let mut found = found_in(bytes, &parts).into_iter();
    literal
        .into_iter()
        .filter(|a| {
            a.text
                .split(" · ")
                .filter(|_| !found.next().expect("an answer per part"))
                .count()
                > 0
        })
        .map(|a| a.id)
        .collect()
}

fn literal_anchors_are_in_the_store(program: &str, env: &str, table: &[Anchor]) -> Outcome {
    let Some((path, bytes)) = store_binary(program, env) else {
        return Outcome::CouldNotRun(format!(
            "CANARY NOT RUN: no {program} build (set {env} or `aterm pkg install \
             {program}`); anchors not checked"
        ));
    };
    literal_anchors_in_build(&path, &bytes, table)
}

fn literal_anchors_in_build(path: &str, bytes: &[u8], table: &[Anchor]) -> Outcome {
    // AN ANCHOR IS OWED ONLY BY THE BUILDS IT WAS WRITTEN FOR: each carries
    // the version it was measured on (`since`), and a store holding an older
    // build is not a build that lost it; one a later build no longer draws
    // carries the last that does (`last`, [`Anchor::drawn_by`]). A version
    // that cannot be read owes every anchor the newest build draws.
    let version = build_version(path);
    let (owed, other): (Vec<Anchor>, Vec<Anchor>) = table
        .iter()
        .copied()
        .partition(|a| a.drawn_by(version.as_deref()));
    if owed.iter().all(|anchor| anchor.kind != AnchorKind::Literal) {
        return Outcome::CouldNotRun(format!(
            "{path} ({}): this table owes no literal anchors to this build; no anchors checked",
            version.as_deref().unwrap_or("unknown version")
        ));
    }
    if !other.is_empty() {
        eprintln!(
            "{path}: {} anchor(s) are for builds other than {}, not checked: {:?}",
            other.len(),
            version.as_deref().unwrap_or("?"),
            other.iter().map(|a| a.id).collect::<Vec<_>>()
        );
    }
    let missing = missing(bytes, &owed);
    assert!(
        missing.is_empty(),
        "{path}: {} literal anchors missing: {missing:?}",
        missing.len()
    );
    eprintln!(
        "{path} ({}): every literal anchor is present",
        version
            .as_deref()
            .unwrap_or("no store record: every anchor owed")
    );
    Outcome::Pass(format!(
        "{path} ({}): literal anchors checked",
        version.as_deref().unwrap_or("unknown version")
    ))
}

/// Where the Gemini CLI canary looks: `$ATERM_GEMINI_BIN`, else the first
/// `gemini` on PATH, else Homebrew's link.
fn gemini_bin() -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("ATERM_GEMINI_BIN") {
        return Some(path.into());
    }
    std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|d| d.join("gemini"))
        .chain(["/opt/homebrew/bin/gemini", "/usr/local/bin/gemini"].map(Into::into))
        .find(|p| p.is_file())
}

fn every_literal_anchor_is_in_the_store_claude() -> Outcome {
    literal_anchors_are_in_the_store("claude", "ATERM_CLAUDE_BIN", ANCHORS)
}
/// The step fold's keys ([`TRANSCRIPT_KEYS`]) in the store's Claude Code
/// build: a build that renamed one writes rows the fold misreads. The
/// build is grepped, never started.
fn every_transcript_key_is_in_the_store_claude() -> Outcome {
    literal_anchors_are_in_the_store("claude", "ATERM_CLAUDE_BIN", TRANSCRIPT_KEYS)
}
/// The Codex step fold's keys ([`ROLLOUT_KEYS`]) in the store's Codex
/// build (`$ATERM_CODEX_BIN`, else `aterm pkg which codex`; its version
/// from atpkg's record, the build never started), as Claude Code's
/// transcript keys are: a build that renamed one writes rows the fold
/// misreads. `CANARY NOT RUN` where no build is found. NEGATIVE
/// CONTROL: where one is, a planted key no build holds is named missing.
fn every_rollout_key_is_in_the_store_codex() -> Outcome {
    let Some((path, bytes)) = store_binary("codex", "ATERM_CODEX_BIN") else {
        return Outcome::CouldNotRun(
            "no codex build (set ATERM_CODEX_BIN or `aterm pkg install codex`); rollout keys not checked".into(),
        );
    };
    let outcome = literal_anchors_in_build(&path, &bytes, ROLLOUT_KEYS);
    if !matches!(outcome, Outcome::Pass(_)) {
        return outcome;
    }
    assert_eq!(
        missing(&bytes, PLANTED_ROLLOUT_KEY),
        ["rollout.planted"],
        "{path}: the planted key"
    );
    outcome
}

/// The drift check for [`reset_templates`], admitted by [`store_binary`]
/// and owed by build as the table's
/// rows are ([`Anchor::drawn_by`]).
fn every_reset_template_is_whole_in_the_store_claude() -> Outcome {
    let Some((path, bytes)) = store_binary("claude", "ATERM_CLAUDE_BIN") else {
        return Outcome::CouldNotRun(
            "CANARY NOT RUN: no claude build found (set ATERM_CLAUDE_BIN, or install one: \
             `aterm pkg install claude`) — the reset's templates are NOT checked against a \
             build"
                .into(),
        );
    };
    let version = build_version(&path);
    if !reset_templates()
        .iter()
        .any(|template| template.spelled_by(version.as_deref()))
    {
        return Outcome::CouldNotRun(format!(
            "{path} ({}): no reset templates apply to this build; no templates checked",
            version.as_deref().unwrap_or("unknown version")
        ));
    }
    let missing = missing_templates(&bytes, version.as_deref());
    assert!(
        missing.is_empty(),
        "{path} ({}): {} reset templates not whole: {missing:?}",
        version.as_deref().unwrap_or("?"),
        missing.len()
    );
    eprintln!(
        "{path} ({}): every reset template is whole",
        version.as_deref().unwrap_or("?")
    );
    Outcome::Pass(format!(
        "{path} ({}): reset templates checked",
        version.as_deref().unwrap_or("unknown version")
    ))
}
fn every_literal_agy_anchor_is_in_the_store_agy() -> Outcome {
    literal_anchors_are_in_the_store("agy", "ATERM_AGY_BIN", AGY_ANCHORS)
}
fn every_literal_muse_anchor_is_in_the_store_muse() -> Outcome {
    literal_anchors_are_in_the_store("muse", "ATERM_MUSE_BIN", MUSE_ANCHORS)
}
/// Gemini's npm package is read as files, including every adjacent .js chunk.
/// Absence is a typed refusal, like any other missing vendor build.
fn every_literal_gemini_anchor_is_in_its_bundle() -> Outcome {
    let Some((dir, bytes, version)) = gemini_bin().as_deref().and_then(gemini_bundle) else {
        return Outcome::CouldNotRun(
            "no Gemini CLI bundle (set ATERM_GEMINI_BIN to its `gemini`, or \
             `brew install gemini-cli`); anchors not checked"
                .into(),
        );
    };
    let (owed, newer): (Vec<Anchor>, Vec<Anchor>) = GEMINI_ANCHORS
        .iter()
        .copied()
        .partition(|a| version.as_deref().is_none_or(|v| !newer_than(a.since, v)));
    if owed.iter().all(|anchor| anchor.kind != AnchorKind::Literal) {
        return Outcome::CouldNotRun(format!(
            "{dir} ({}): no literal Gemini anchors apply to this build; no anchors checked",
            version.as_deref().unwrap_or("unknown version")
        ));
    }
    if !newer.is_empty() {
        eprintln!(
            "{dir}: {} anchor(s) are for builds newer than {}, not checked: {:?}",
            newer.len(),
            version.as_deref().unwrap_or("?"),
            newer.iter().map(|a| a.id).collect::<Vec<_>>()
        );
    }
    let missing = missing(&bytes, &owed);
    assert!(
        missing.is_empty(),
        "{dir}: {} literal anchors missing: {missing:?}",
        missing.len()
    );
    eprintln!(
        "{dir} ({}): every literal Gemini CLI anchor is present",
        version
            .as_deref()
            .unwrap_or("no package version: every anchor owed")
    );
    Outcome::Pass(format!(
        "{dir} ({}): Gemini anchors checked",
        version.as_deref().unwrap_or("unknown version")
    ))
}

/// THE DRIFT CANARY FOR THE PHRASE TABLE. The reader names a wall by
/// [`PHRASES`], and a vendor build that words a notice differently reads
/// as no wall — behind a shell, as `busy` for as long as the shell runs.
/// Every row (lowercased) must be in the
/// store claude's binary, but for the rows older builds carry and neither
/// 2.1.283 nor 2.1.284 does (measured). The explicit bench resolves
/// `$ATERM_CLAUDE_BIN` or `aterm pkg which claude`; absence is CouldNotRun.
fn every_wall_phrase_is_in_the_store_claude() -> Outcome {
    /// Rows of [`PHRASES`] that older builds carry and 2.1.283 and 2.1.284
    /// do not (`strings -a` of both, 2026-09-28).
    const OLDER_BUILDS: &[&str] = &[
        "repeated overloaded errors",
        "weekly rate limit",
        "seven-day limit",
        "haiku limit",
        "reached your limit",
        "limit will reset",
    ];
    /// Rows whose words are the API's own, which Claude Code only passes
    /// on (`API Error: <its message>`): no build carries them (anchors.rs,
    /// `wall.refused.identity`, `wall.refused.dlp`).
    const SERVER_WORDS: &[&str] = &["verify your identity", "data loss prevention"];
    let Some((path, bytes)) = store_binary("claude", "ATERM_CLAUDE_BIN") else {
        return Outcome::CouldNotRun(
            "CANARY NOT RUN: no claude build found (set ATERM_CLAUDE_BIN, or `aterm pkg \
             install claude`) — the wall phrases are NOT checked against a build"
                .into(),
        );
    };
    let lower = bytes.to_ascii_lowercase();
    // The vendor draws `\u{b7}` and `\u{2019}` from escapes, and drops
    // number tokens from a notice's head before the table reads it.
    let owed: Vec<&str> = PHRASES
        .iter()
        .map(|(p, _)| *p)
        .filter(|p| !OLDER_BUILDS.contains(p) && !SERVER_WORDS.contains(p))
        .collect();
    if owed.is_empty() {
        return Outcome::CouldNotRun(format!(
            "{path} ({}): no wall phrases apply after the table's exclusions; no phrases checked",
            build_version(&path).as_deref().unwrap_or("unknown version")
        ));
    }
    let curled: Vec<String> = owed.iter().map(|p| p.replace('\'', "\u{2019}")).collect();
    let needles: Vec<&[u8]> = owed
        .iter()
        .map(|p| p.as_bytes())
        .chain(curled.iter().map(String::as_bytes))
        .collect();
    let found = found_in(&lower, &needles);
    let (plain, curly) = found.split_at(owed.len());
    let missing: Vec<&str> = owed
        .iter()
        .zip(plain.iter().zip(curly))
        .filter(|(_, (plain, curly))| !**plain && !**curly)
        .map(|(p, _)| *p)
        .collect();
    assert!(
        missing.is_empty(),
        "wall phrases the store claude no longer carries: {missing:?}"
    );
    Outcome::Pass("Claude wall phrases checked".into())
}
