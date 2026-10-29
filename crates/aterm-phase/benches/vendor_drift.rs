// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! On-demand vendor-file drift acceptance benchmark.

use anchors::*;
use aterm_phase::anchors;
use aterm_phase::reset;
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
        "every_composed_template_is_whole_in_the_store_claude",
        every_composed_template_is_whole_in_the_store_claude,
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
        "every_literal_grok_anchor_is_in_the_store_grok",
        every_literal_grok_anchor_is_in_the_store_grok,
    ),
    (
        "every_literal_kimi_anchor_is_in_the_store_kimi",
        every_literal_kimi_anchor_is_in_the_store_kimi,
    ),
    (
        "every_wall_phrase_is_in_the_store_claude",
        every_wall_phrase_is_in_the_store_claude,
    ),
];

fn main() -> ExitCode {
    support::run("phase_vendor_drift", ROWS, |check| check())
}

/// Whether `bytes` are a native vendor executable rather than a stub script.
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
const MIN_BUILD_BYTES: usize = 4 * 1024 * 1024;

/// Resolve [`canary_path`] for `program` from `$<env>` or `aterm pkg which`.
pub(crate) fn canary_binary(program: &str, env: &str) -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os(env) {
        return Some(path.into());
    }
    let out = aterm_uds::spawnfd::command("aterm")
        .args(["pkg", "which", program])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    canary_path(text.lines().next()?, home.as_deref())
}

fn store_binary(program: &str, env: &str) -> Option<(String, Vec<u8>)> {
    let path = canary_binary(program, env)?.to_string_lossy().into_owned();
    let bytes = std::fs::read(&path).ok()?;
    if !looks_like_a_build(&bytes) {
        eprintln!("{path} is a stub rather than a vendor build");
        return None;
    }
    Some((path, bytes))
}

/// Which of `needles` occur in `hay`, scanned in a single pass.
pub(crate) fn found_in(hay: &[u8], needles: &[&[u8]]) -> Vec<bool> {
    const HEAD: usize = 3;
    let mut found: Vec<bool> = needles.iter().map(|n| n.is_empty()).collect();
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

/// The literal anchors of `table` missing from `bytes`.
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

fn every_literal_anchor_is_in_the_store_claude() -> Outcome {
    literal_anchors_are_in_the_store("claude", "ATERM_CLAUDE_BIN", ANCHORS)
}
fn every_transcript_key_is_in_the_store_claude() -> Outcome {
    literal_anchors_are_in_the_store("claude", "ATERM_CLAUDE_BIN", TRANSCRIPT_KEYS)
}
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

fn every_reset_template_is_whole_in_the_store_claude() -> Outcome {
    templates_are_whole_in_the_store_claude("reset", "the reset's templates", reset_templates())
}

fn every_composed_template_is_whole_in_the_store_claude() -> Outcome {
    templates_are_whole_in_the_store_claude(
        "composed",
        "the composed anchors' templates",
        composed_templates(),
    )
}

fn templates_are_whole_in_the_store_claude(
    kind: &str,
    what: &str,
    templates: Vec<(&'static str, Vec<String>)>,
) -> Outcome {
    let Some((path, bytes)) = store_binary("claude", "ATERM_CLAUDE_BIN") else {
        return Outcome::CouldNotRun(format!(
            "CANARY NOT RUN: no claude build found (set ATERM_CLAUDE_BIN, or install one: \
             `aterm pkg install claude`) — {what} are NOT checked against a build"
        ));
    };
    let version = build_version(&path);
    let missing = missing_templates(&bytes, &templates);
    assert!(
        missing.is_empty(),
        "{path} ({}): {} {kind} templates not whole: {missing:?}",
        version.as_deref().unwrap_or("?"),
        missing.len()
    );
    let changed: Vec<(&str, Vec<String>)> = templates
        .iter()
        .map(|(name, spellings)| (*name, spellings.iter().flat_map(|t| reworded(t)).collect()))
        .collect();
    let refused = missing_templates(&bytes, &changed);
    let held: Vec<&str> = changed
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !refused.contains(name))
        .collect();
    assert!(
        held.is_empty(),
        "{path}: {} {kind} templates are held with a word changed: {held:?}",
        held.len()
    );
    eprintln!(
        "{path} ({}): every {kind} template is whole ({}), and none with a word changed ({})",
        version.as_deref().unwrap_or("?"),
        templates.len(),
        changed
            .iter()
            .map(|(_, spellings)| spellings.len())
            .sum::<usize>()
    );
    Outcome::Pass(format!(
        "{path} ({}): {kind} templates checked",
        version.as_deref().unwrap_or("unknown version")
    ))
}
fn every_literal_agy_anchor_is_in_the_store_agy() -> Outcome {
    literal_anchors_are_in_the_store("agy", "ATERM_AGY_BIN", AGY_ANCHORS)
}
fn every_literal_muse_anchor_is_in_the_store_muse() -> Outcome {
    literal_anchors_are_in_the_store("muse", "ATERM_MUSE_BIN", MUSE_ANCHORS)
}
/// Every literal Grok Build anchor is in the store's grok. NEGATIVE CONTROL: each login-ended
/// text (`grok.auth.*`, which aterm-agent reads its login-ended words from) with any one word
/// changed is in no build, so the check tells a text the build carries from one it does not.
fn every_literal_grok_anchor_is_in_the_store_grok() -> Outcome {
    let Some((path, bytes)) = store_binary("grok", "ATERM_GROK_BIN") else {
        return Outcome::CouldNotRun(
            "CANARY NOT RUN: no grok build (set ATERM_GROK_BIN or `aterm pkg install grok`); \
             anchors not checked"
                .into(),
        );
    };
    let outcome = literal_anchors_in_build(&path, &bytes, GROK_ANCHORS);
    if !matches!(outcome, Outcome::Pass(_)) {
        return outcome;
    }
    let version = build_version(&path);
    let mutants: Vec<(&str, String)> = GROK_ANCHORS
        .iter()
        .filter(|a| a.id.starts_with("grok.auth.") && a.drawn_by(version.as_deref()))
        .flat_map(|a| reworded(a.text).into_iter().map(move |m| (a.id, m)))
        .collect();
    assert!(!mutants.is_empty(), "{path}: no login text to mutate");
    let needles: Vec<&[u8]> = mutants.iter().map(|(_, m)| m.as_bytes()).collect();
    let held: Vec<&(&str, String)> = mutants
        .iter()
        .zip(found_in(&bytes, &needles))
        .filter(|(_, found)| *found)
        .map(|(m, _)| m)
        .collect();
    assert!(
        held.is_empty(),
        "{path}: {} login texts are held with a word changed: {held:?}",
        held.len()
    );
    eprintln!(
        "{path}: no login text is held with a word changed ({} mutants)",
        mutants.len()
    );
    outcome
}
fn every_literal_kimi_anchor_is_in_the_store_kimi() -> Outcome {
    literal_anchors_are_in_the_store("kimi", "ATERM_KIMI_BIN", KIMI_ANCHORS)
}

fn every_wall_phrase_is_in_the_store_claude() -> Outcome {
    const OLDER_BUILDS: &[&str] = &[
        "repeated overloaded errors",
        "weekly rate limit",
        "seven-day limit",
        "haiku limit",
        "reached your limit",
        "limit will reset",
    ];
    const SERVER_WORDS: &[&str] = &["verify your identity", "data loss prevention"];
    // A phrase a build first drew, owed from that build on (an anchor's `since`).
    const SINCE: &[(&str, &str)] = &[
        ("5-hour limit reset", "2.1.312"),
        ("reset your 5-hour limit", "2.1.312"),
        ("resetting your 5-hour limit", "2.1.312"),
        ("5-hour limit is already being reset", "2.1.312"),
    ];
    // A phrase and the last build that drew it, owed through that build (2.1.312 says
    // `5-hour limit`); a build of no known version is taken for the newest, which draws none.
    const UNTIL: &[(&str, &str)] = &[
        ("session limit reset", "2.1.311"),
        ("reset your session limit", "2.1.311"),
        ("resetting your session limit", "2.1.311"),
        ("session limit is already being reset", "2.1.311"),
    ];
    let Some((path, bytes)) = store_binary("claude", "ATERM_CLAUDE_BIN") else {
        return Outcome::CouldNotRun(
            "CANARY NOT RUN: no claude build found (set ATERM_CLAUDE_BIN, or `aterm pkg \
             install claude`) — the wall phrases are NOT checked against a build"
                .into(),
        );
    };
    let version = build_version(&path);
    let drawn = |phrase: &str| {
        SINCE.iter().all(|&(p, since)| {
            p != phrase || version.as_deref().is_none_or(|v| !newer_than(since, v))
        }) && UNTIL.iter().all(|&(p, until)| {
            p != phrase || version.as_deref().is_some_and(|v| !newer_than(v, until))
        })
    };
    let lower = bytes.to_ascii_lowercase();
    let owed: Vec<&str> = PHRASES
        .iter()
        .map(|(p, _)| *p)
        .filter(|p| !OLDER_BUILDS.contains(p) && !SERVER_WORDS.contains(p) && drawn(p))
        .collect();
    if owed.is_empty() {
        return Outcome::CouldNotRun(format!(
            "{path} ({}): no wall phrases apply after the table's exclusions; no phrases checked",
            version.as_deref().unwrap_or("unknown version")
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
