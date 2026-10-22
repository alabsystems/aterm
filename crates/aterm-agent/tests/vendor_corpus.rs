// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vendor corpus replay: every fixture under `tests/testdata/vendor/` is
//! bytes a real Claude Code sent or painted, and this suite drives the shipping
//! harness readers over them offline.
//!
//! Every other test hands the harness a payload its author wrote; the corpus
//! is the vendor's own, recorded from a live vendor build (`README.md` beside
//! it says how), and this suite replays it with no vendor installed.
//!
//! A green run means: every Bash command the vendor build was observed to ask
//! a permission for is pressed at full power when drawn into its box, and the
//! painted `/usage` panel yields its windows. The hook payloads are data, not
//! a channel: the vendor's record of the commands a session runs. Each replay
//! fails on a corpus that gives it nothing to replay.

use std::fs;
use std::path::{Path, PathBuf};

/// Where the corpus lives, relative to this crate.
const CORPUS: &str = "tests/testdata/vendor";

// Reading the corpus

/// One captured payload: the bytes, and where they came from.
#[derive(Debug, Clone)]
struct Fixture {
    /// The corpus-relative path, e.g. `2.1.278/PermissionRequest/0.json`.
    rel: String,
    /// `PermissionRequest` or `screen`.
    kind: String,
    bytes: Vec<u8>,
}

impl Fixture {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

/// One captured vendor build.
#[derive(Debug)]
struct Capture {
    /// The version directory's name, which is the vendor version.
    version: String,
    fixtures: Vec<Fixture>,
}

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(CORPUS)
}

/// Every captured build in the corpus, newest directory name last.
fn captures() -> Vec<Capture> {
    let root = corpus_root();
    let Ok(entries) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.into_iter()
        .map(|dir| {
            let version = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut fixtures = Vec::new();
            collect(&dir, &dir, &mut fixtures);
            fixtures.sort_by(|a, b| a.rel.cmp(&b.rel));
            Capture { version, fixtures }
        })
        .collect()
}

fn collect(base: &Path, dir: &Path, out: &mut Vec<Fixture>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect(base, &path, out);
            continue;
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if name == "manifest.toml" || name.starts_with('.') {
            continue;
        }
        // Spelled with `/` on every host, as the corpus names its files.
        let rel = path
            .strip_prefix(base)
            .unwrap_or(&path)
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let kind = rel.split('/').next().unwrap_or_default().to_string();
        let Ok(bytes) = fs::read(&path) else { continue };
        out.push(Fixture { rel, kind, bytes });
    }
}

// The replay

/// Full power over the commands the vendor really asked about. Every
/// `PermissionRequest` payload carries the command its Bash box asked about;
/// drawn into that box (hand-built around the recorded command on the 2.1.280
/// Bash box's geometry), the default presses its `1`, and the safe rules alone
/// (`approve = "safe"`) press nothing full power would not. Non-vacuity: at
/// least one box is pressed that the safe rules hand over.
#[test]
fn every_captured_permission_request_is_pressed_at_full_power() {
    use aterm_agent::supervise::config::Approve;
    use aterm_agent::supervise::policy::{
        ApprovalCtx, Choice, Decision, RULE_ALLOW_ONCE, decide_screen,
    };
    let dir = corpus_root();
    let ctx = |approve: Approve| ApprovalCtx {
        approve,
        ..ApprovalCtx::new(
            dir.join("work"),
            Some(dir.join("home")),
            502,
            None,
            std::sync::Arc::new(aterm_agent::supervise::policy::LiveProbe),
        )
    };
    let mut asked = 0usize;
    let mut unproven = 0usize;
    for cap in captures() {
        for f in cap
            .fixtures
            .iter()
            .filter(|f| f.kind == "PermissionRequest")
        {
            let Ok(payload) = aterm_json::from_str::<aterm_json::Value>(&f.text()) else {
                continue;
            };
            let Some(cmd) = payload
                .get("tool_input")
                .and_then(|i| i.get("command"))
                .and_then(|v| v.as_str())
            else {
                continue;
            };
            let mut rows = vec!["─".repeat(120), " Bash command".to_string(), String::new()];
            rows.extend(cmd.lines().map(|l| format!("   {l}")));
            rows.extend(
                [
                    "",
                    " Do you want to proceed?",
                    " ❯ 1. Yes",
                    "   2. No",
                    "",
                    " Esc to cancel · Tab to amend",
                ]
                .map(str::to_string),
            );
            let every = decide_screen(Some("claude"), &rows, &ctx(Approve::All))
                .unwrap_or_else(|| panic!("{}/{}: no box read for {cmd:?}", cap.version, f.rel));
            let Decision::Approve {
                rule_id, choice, ..
            } = &every
            else {
                panic!(
                    "{}/{}: full power did not press {cmd:?}: {every:?}",
                    cap.version, f.rel
                );
            };
            assert_eq!(*choice, Choice::Digit(1), "{}/{}", cap.version, f.rel);
            asked += 1;
            let proven = decide_screen(Some("claude"), &rows, &ctx(Approve::Safe));
            match proven {
                Some(Decision::Approve { rule_id: p, .. }) => assert_eq!(
                    p, *rule_id,
                    "{}/{}: a proven box keeps its rule at full power",
                    cap.version, f.rel
                ),
                _ => {
                    assert_eq!(*rule_id, RULE_ALLOW_ONCE, "{}/{}", cap.version, f.rel);
                    unproven += 1;
                }
            }
        }
    }
    assert!(
        asked > 0,
        "the corpus carries no PermissionRequest with a command"
    );
    assert!(
        unproven > 0,
        "{asked} box(es) replayed and every one was proven, so nothing here shows full power \
         pressing what the safe rules hand over. Capture a turn that asks to write."
    );
}

/// The painted screen is rank-1 evidence, and here it is a real paint: the
/// `/usage` panel is the only place some windows (the model bucket among them)
/// appear, and `harness limits`' reader over the vendor's paint yields every
/// window the panel painted.
///
/// Negative control: the same reader over the session's own `status` line (a
/// real capture that is not a usage panel) yields nothing.
#[test]
fn the_painted_usage_panel_yields_its_windows() {
    use aterm_agent::harness::cli::limits_view;

    let rows = |s: &str| -> Vec<String> { s.lines().map(str::to_string).collect() };
    let zone_none = |_: &str| -> Option<i64> { None };
    let mut panels = 0usize;
    for cap in captures() {
        for f in cap.fixtures.iter().filter(|f| f.kind == "screen") {
            let text = f.text();
            let got = limits_view(
                aterm_phase::Program::Claude.reader(),
                &rows(&text),
                1_790_000_000,
                0,
                &|z, _| zone_none(z),
            )
            .windows;
            if f.rel.ends_with("usage.txt") {
                let kinds: Vec<&str> = got.iter().map(|(w, _)| w.name.as_str()).collect();
                assert!(
                    !kinds.is_empty(),
                    "{}/{}: the captured /usage panel yielded no window — either the \
                     vendor repainted this page or the grid reader stopped seeing it. The panel \
                     is:\n{text}",
                    cap.version,
                    f.rel
                );
                // The model bucket is the point: it exists on no other source.
                assert!(
                    kinds.contains(&"seven_day_overage_included"),
                    "{}/{}: the panel paints `Current week (Fable)` and the reader did not \
                     produce `seven_day_overage_included`. That window appears on no other \
                     source (design 5.2), so losing it here loses it entirely. Kinds: {kinds:?}",
                    cap.version,
                    f.rel
                );
                panels += 1;
            } else if f.rel.ends_with("usage.status") {
                assert!(
                    got.is_empty(),
                    "{}/{}: the window reader found windows in a `status` line, so finding them \
                     in the panel proves nothing: {got:?}",
                    cap.version,
                    f.rel
                );
            }
        }
    }
    assert!(
        panels > 0,
        "no painted /usage panel in the corpus, so the rank-1 window path was never replayed"
    );
}
