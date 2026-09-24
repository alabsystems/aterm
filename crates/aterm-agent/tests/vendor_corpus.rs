// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The VENDOR CORPUS replay: every fixture under `tests/testdata/vendor/` is
//! bytes a real Claude Code really sent or really painted, and this suite
//! drives the shipping harness readers over them offline.
//!
//! # Why this suite exists, in one paragraph
//!
//! We do not control Claude Code, and the owner asked the question this suite
//! answers: *how would you know for sure that these situations are handled?*
//! Every other test in this tree hands the harness a payload its author
//! wrote, so a green run says the harness handles what its author imagined.
//! The corpus is the other half. `tools/harness-capture.sh` drives a live
//! vendor build, tees the bytes it sends into files, records WHICH build sent
//! them, and checks the result in; this suite replays them with no vendor
//! installed, in every clone, on every gate run. When the vendor changes
//! shape, re-capture and this suite says whether the harness still holds.
//!
//! # What a green run here does and does not mean
//!
//! It means: for every Bash command this vendor build was OBSERVED to send,
//! the rm policy reaches a verdict, and at least one of them is an allow; the
//! vendor's own statusLine payloads read into one HUD line carrying their
//! figures; and the painted `/usage` panel yields window evidence. It does NOT
//! mean the vendor will not send something else tomorrow — nothing can mean
//! that. It means the day it does, re-capturing makes this suite say so.
//!
//! The hook payloads are DATA now, not a channel: decision "B" (2026-09-22)
//! retired the hook bridge, and its reply builder went with the second
//! harness stack on 2026-09-23. They stay because they are the vendor's own
//! record of the commands a session runs and of its rate-limit windows.
//!
//! # Non-vacuity
//!
//! An empty corpus would make every assertion below true of nothing, which is
//! the exact failure this suite is here to prevent elsewhere. So
//! [`the_corpus_is_not_empty_and_covers_the_events_that_decide`] fails on a
//! corpus that is missing, empty, or missing either event that carries the
//! Bash command a permission box asks about. Deleting the fixtures does not
//! make this suite pass.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use aterm_agent::harness::rm_policy::{CwdPrefix, HookEvent, RmDecision, RmPolicy, evaluate};
use aterm_agent::harness::usage::{self, AccountView, UsageView};
use aterm_digest::Sha256;

/// Where the corpus lives, relative to this crate.
const CORPUS: &str = "tests/testdata/vendor";

/// The events whose stdout the vendor READS. A corpus with no payload for one
/// of these has a hole exactly where the harness makes a decision, so the
/// non-vacuity test below refuses it.
const DECIDE_EVENTS: &[&str] = &["PermissionRequest", "PreToolUse"];

/// The statusLine's directory name. It is the EVENT name the vendor's
/// bridge is invoked with (`bridge.sh statusline StatusLine usage-hud`),
/// not the mode word — capitalised, like every other row of `HOOK_ROWS`.
const STATUSLINE: &str = "StatusLine";

// ---------------------------------------------------------------------------
// Reading the corpus
// ---------------------------------------------------------------------------

/// One captured payload: the bytes, and where they came from.
#[derive(Debug, Clone)]
struct Fixture {
    /// The corpus-relative path, e.g. `2.1.278/PermissionRequest/0.json`.
    rel: String,
    /// `PermissionRequest`, `statusline`, `screen`, …
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
    dir: PathBuf,
    /// `key = value` lines of `manifest.toml`, before the `[files]` table.
    meta: BTreeMap<String, String>,
    /// `path -> (bytes, sha256)` from the manifest's `[files]` table.
    claimed: BTreeMap<String, (u64, String)>,
    fixtures: Vec<Fixture>,
}

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(CORPUS)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A deliberately small reader for the manifest: it takes `key = "value"` and
/// the `[files]` rows this repository's own capture script writes, and nothing
/// else. A parser with more range would be a second TOML implementation in a
/// tree that already refuses to grow one.
fn read_manifest(path: &Path) -> (BTreeMap<String, String>, BTreeMap<String, (u64, String)>) {
    let text = fs::read_to_string(path).unwrap_or_default();
    let mut meta = BTreeMap::new();
    let mut files = BTreeMap::new();
    let mut in_files = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[files]" {
            in_files = true;
            continue;
        }
        let Some((key, rest)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"').to_string();
        let rest = rest.trim();
        if in_files {
            let bytes = rest
                .split("bytes")
                .nth(1)
                .and_then(|s| s.trim_start_matches([' ', '=']).split(',').next())
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(0);
            let sha = rest
                .split("sha256")
                .nth(1)
                .and_then(|s| s.split('"').nth(1))
                .unwrap_or_default()
                .to_string();
            files.insert(key, (bytes, sha));
        } else {
            meta.insert(key, rest.trim_matches('"').to_string());
        }
    }
    (meta, files)
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
            let (meta, claimed) = read_manifest(&dir.join("manifest.toml"));
            let mut fixtures = Vec::new();
            collect(&dir, &dir, &mut fixtures);
            fixtures.sort_by(|a, b| a.rel.cmp(&b.rel));
            Capture {
                version,
                dir,
                meta,
                claimed,
                fixtures,
            }
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
        let rel = path
            .strip_prefix(base)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        let kind = rel.split('/').next().unwrap_or_default().to_string();
        let Ok(bytes) = fs::read(&path) else { continue };
        out.push(Fixture { rel, kind, bytes });
    }
}

// ---------------------------------------------------------------------------
// The non-vacuity floor
// ---------------------------------------------------------------------------

/// THE FLOOR. Every assertion in this file is over the corpus, so an absent or
/// empty corpus would make all of them vacuously true — the failure mode this
/// suite exists to prevent. This is the one test that fails when there is
/// nothing to replay.
#[test]
fn the_corpus_is_not_empty_and_covers_the_events_that_decide() {
    let caps = captures();
    assert!(
        !caps.is_empty(),
        "no vendor capture under {} — tools/harness-capture.sh records one, but its live mode \
         is suspended while `aterm harness install` is retired (decision B); `--from-raw` \
         rebuilds from a kept raw/ tree. A green run of this suite over an empty corpus would \
         mean nothing.",
        corpus_root().display()
    );
    for cap in &caps {
        assert!(
            !cap.fixtures.is_empty(),
            "{}: the capture directory holds no payloads",
            cap.version
        );
        // EVERY PAYLOAD MUST BE A PAYLOAD. Without this, `touch
        // PermissionRequest/0.json` plus a regenerated manifest satisfies the
        // whole suite: the kinds below are DIRECTORY names, and an empty file
        // has a perfectly good digest. So each hook fixture must parse, must
        // carry the session id every vendor payload carries, and must name in
        // its own body the event its directory claims — which is also what
        // catches a payload filed under the wrong directory.
        for f in &cap.fixtures {
            if f.kind == "screen" {
                continue;
            }
            let parsed: aterm_json::Value = aterm_json::from_str(&f.text())
                .unwrap_or_else(|e| panic!("{}/{}: not JSON ({e})", cap.version, f.rel));
            assert!(
                parsed.get("session_id").and_then(|v| v.as_str()).is_some(),
                "{}/{}: no session_id — a stub file is not a capture",
                cap.version,
                f.rel
            );
            if f.kind != STATUSLINE {
                assert_eq!(
                    parsed.get("hook_event_name").and_then(|v| v.as_str()),
                    Some(f.kind.as_str()),
                    "{}/{}: the payload names a different event than its directory",
                    cap.version,
                    f.rel
                );
            }
        }
        let kinds: BTreeSet<&str> = cap.fixtures.iter().map(|f| f.kind.as_str()).collect();
        for want in DECIDE_EVENTS {
            assert!(
                kinds.contains(want),
                "{}: no `{want}` payload captured, so the decide channel this harness exists to \
                 answer was never exercised. Capture in `--permission-mode manual`: an `auto` \
                 session never sends PermissionRequest. Kinds present: {kinds:?}",
                cap.version
            );
        }
        assert!(
            kinds.contains(STATUSLINE),
            "{}: no statusLine payload captured, so the HUD path is untested here. Kinds: {kinds:?}",
            cap.version
        );
    }
}

/// The corpus cannot rot silently: every file is the size and the digest the
/// manifest recorded at capture time, and the manifest names every file on
/// disk. An edited fixture is an edited fixture, not a vendor observation.
#[test]
fn every_fixture_is_the_bytes_the_manifest_recorded() {
    for cap in captures() {
        assert!(
            !cap.claimed.is_empty(),
            "{}: manifest.toml claims no files — provenance is the point of this corpus",
            cap.version
        );
        for want in ["vendor_version", "captured_utc", "vendor"] {
            assert!(
                cap.meta.contains_key(want),
                "{}: manifest.toml has no `{want}`",
                cap.version
            );
        }
        assert_eq!(
            cap.meta.get("vendor_version").map(String::as_str),
            Some(cap.version.as_str()),
            "{}: the directory name and the manifest's vendor_version disagree",
            cap.version
        );

        let on_disk: BTreeSet<&str> = cap.fixtures.iter().map(|f| f.rel.as_str()).collect();
        let claimed: BTreeSet<&str> = cap.claimed.keys().map(String::as_str).collect();
        assert_eq!(
            on_disk, claimed,
            "{}: the manifest and the directory disagree about which files exist",
            cap.version
        );

        for f in &cap.fixtures {
            let (bytes, sha) = &cap.claimed[&f.rel];
            assert_eq!(
                *bytes,
                f.bytes.len() as u64,
                "{}/{}: byte count moved since capture",
                cap.version,
                f.rel
            );
            assert_eq!(
                *sha,
                hex(&Sha256::digest(&f.bytes)),
                "{}/{}: contents changed since capture — a fixture is an OBSERVATION, not a \
                 file to edit. Re-capture instead.",
                cap.version,
                f.rel
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The replay
// ---------------------------------------------------------------------------

/// THE POLICY DECIDES REAL COMMANDS, and says yes to at least one.
///
/// Until 2026-09-23 this law compared `rm_policy::evaluate` with the hook
/// bridge's reply over the same payloads. The bridge is retired (decision
/// "B") and deleted; the policy is what the supervisor's rm breaker asks
/// (`supervise::policy::rm_breaker`), so what is replayed now is the policy
/// itself over every Bash command the vendor really sent — built the way the
/// bridge built it (the payload's own cwd, `home`, the `SessionCwd` prefix),
/// so a verdict here is a verdict about the COMMAND.
///
/// Its non-vacuity counter counts ALLOWS — the outcome the policy exists for
/// — not mentions of two letters, which `npm run format` would satisfy.
#[test]
fn every_captured_bash_command_reaches_a_policy_verdict() {
    let dir = corpus_root();
    let fallback_cwd = dir.join("work");
    let policy = RmPolicy {
        home: Some(dir.join("home")),
        require_cwd_prefix: CwdPrefix::SessionCwd,
        ..RmPolicy::default()
    };
    let mut compared = 0usize;
    let mut allowed = 0usize;
    for cap in captures() {
        for f in &cap.fixtures {
            let Some(event) = HookEvent::parse(&f.kind) else {
                continue;
            };
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
            // The PAYLOAD's cwd when it is absolute, as the bridge read it.
            let cwd = payload
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| fallback_cwd.clone());
            let verdict = evaluate(cmd, &cwd, event, &policy);
            // A verdict always names itself; an allow always says why.
            assert!(
                verdict.decision != RmDecision::Allow || !verdict.reason.is_empty(),
                "{}/{}: an allow with no reason for {cmd:?}",
                cap.version,
                f.rel
            );
            compared += 1;
            if verdict.decision == RmDecision::Allow {
                allowed += 1;
            }
        }
    }
    assert!(
        compared > 0,
        "the corpus carries no Bash tool call at all, so nothing was compared"
    );
    assert!(
        allowed > 0,
        "{compared} command(s) compared and NOT ONE was allowed, so function 1's whole reason \
         for existing — saying yes to a safe rm — was never replayed. Capture a turn that runs \
         `rm <file>` inside the session's own cwd."
    );
}

/// THE HUD LINE, over the vendor's own statusLine payloads.
///
/// Nothing installs a statusLine any more (decision "B"), but the payloads
/// are still the vendor's own record of its windows, and `usage`'s reader is
/// still how aterm reads one: exactly one line, never empty, never a panic,
/// and the five-hour figure reaches it.
#[test]
fn every_captured_statusline_renders_one_footer_line() {
    let mut rendered = 0usize;
    let mut carried = 0usize;
    for cap in captures() {
        for f in cap.fixtures.iter().filter(|f| f.kind == STATUSLINE) {
            let parsed = usage::parse_statusline(&f.text()).unwrap_or_else(|e| {
                panic!(
                    "{}/{}: the vendor's own statusLine did not parse ({e:?})",
                    cap.version, f.rel
                )
            });
            let mut view = UsageView::new(1_790_000_000);
            let mut account = AccountView::new("account", true);
            account.add_statusline(&parsed, 0);
            view.accounts.push(account);
            let line = usage::hud_line(&view, 0);
            assert!(
                !line.is_empty(),
                "{}/{}: the HUD rendered nothing",
                cap.version,
                f.rel
            );
            assert!(
                !line.contains('\n') && !line.contains('\r'),
                "{}/{}: the HUD rendered more than one line: {line:?}",
                cap.version,
                f.rel
            );
            // THE DISCRIMINATING PART: a payload carrying
            // `rate_limits.five_hour.used_percentage` must produce a line
            // naming that percentage. A reader that dropped the payload, or
            // read the wrong window, fails.
            if let Some(pct) = parsed
                .rate_limits
                .as_ref()
                .and_then(|r| r.five_hour())
                .and_then(|w| w.used_pct)
            {
                let want = format!("{}%/5h", pct.round() as i64);
                assert!(
                    line.contains(&want),
                    "{}/{}: the payload carries five_hour at {pct}% and the footer reads {line:?} \
                     — the figure did not reach it",
                    cap.version,
                    f.rel
                );
                carried += 1;
            }
            rendered += 1;
        }
    }
    assert!(rendered > 0, "no statusLine payload was replayed");
    assert!(
        carried > 0,
        "{rendered} statusLine payload(s) replayed and NOT ONE carried a five-hour percentage, \
         so nothing here checked that a figure reaches the footer. Capture a session that has \
         done enough work to have a rate-limit block."
    );
}

/// THE PAINTED SCREEN IS RANK-1 EVIDENCE, and here it is a real paint.
///
/// The design ranks aterm's own view of the grid above every vendor hook
/// (§5.8.1), and the `/usage` panel is the only place some windows — the
/// model bucket among them — appear at all. So the corpus carries what the
/// vendor DREW, and the reader runs over it: every window the panel painted
/// becomes `Evidence::Window` with `Source::Grid`, on bytes nobody wrote by
/// hand.
///
/// The negative control is in the same test rather than a separate one: the
/// same reader over the session's own `status` line — a real capture of
/// something that is NOT a usage panel — must yield nothing, so a reader that
/// found windows everywhere could not pass this.
#[test]
fn the_painted_usage_panel_yields_window_evidence() {
    use aterm_agent::harness::limits::{Evidence, WindowKind};

    let rows = |s: &str| -> Vec<String> { s.lines().map(str::to_string).collect() };
    let zone_none = |_: &str| -> Option<i64> { None };
    let mut panels = 0usize;
    for cap in captures() {
        for f in cap.fixtures.iter().filter(|f| f.kind == "screen") {
            let text = f.text();
            let got = Evidence::windows_from_screen(&rows(&text), 1_790_000_000, 0, zone_none);
            if f.rel.ends_with("usage.txt") {
                let kinds: Vec<WindowKind> = got
                    .iter()
                    .filter_map(|e| match e {
                        Evidence::Window { which, .. } => Some(*which),
                        _ => None,
                    })
                    .collect();
                assert!(
                    !kinds.is_empty(),
                    "{}/{}: the captured /usage panel yielded no window evidence — either the \
                     vendor repainted this page or the grid reader stopped seeing it. The panel \
                     is:\n{text}",
                    cap.version,
                    f.rel
                );
                // THE MODEL BUCKET IS THE POINT. `windows_from_screen` drops a
                // title it cannot map (`WindowKind::parse` returns None and
                // `filter_map` discards it), so a vendor rename of `Current
                // week (Fable)` would silently remove the one window that
                // exists on NO other source — while `five_hour` and
                // `seven_day` kept the old assertion green. This panel painted
                // it, so this panel must still yield it.
                assert!(
                    text.contains("Current week (Fable)"),
                    "{}/{}: this corpus's panel no longer carries the Fable row — re-capture",
                    cap.version,
                    f.rel
                );
                assert!(
                    kinds.contains(&WindowKind::SevenDayOverageIncluded),
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
        "no painted /usage panel in the corpus, so the rank-1 evidence path was never replayed"
    );
}

/// THE VERSION SKEW NOTE — printed, never failed.
///
/// A corpus captured from a build the machine no longer has is still a real
/// observation, and failing on that would turn every vendor update into a red
/// gate for a reason that is not about this tree. But a silent skew is how a
/// corpus quietly stops describing the vendor, so the run SAYS so.
#[test]
fn the_corpus_says_which_vendor_build_it_describes() {
    let caps = captures();
    let installed = std::process::Command::new("claude")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string()
        });
    let versions: Vec<&str> = caps.iter().map(|c| c.version.as_str()).collect();
    println!("vendor corpus: {} capture(s): {versions:?}", caps.len());
    match installed {
        None => println!("vendor corpus: no `claude` on PATH — nothing to compare against"),
        Some(v) if versions.contains(&v.as_str()) => {
            println!("vendor corpus: the installed build {v} IS in the corpus");
        }
        Some(v) => println!(
            "vendor corpus: SKEW — the installed build is {v} and the corpus describes \
             {versions:?}; nothing here has been shown to hold for {v}. Re-recording needs \
             tools/harness-capture.sh's live mode, suspended while `aterm harness install` is \
             retired (decision B)."
        ),
    }
    for cap in &caps {
        println!(
            "  {} captured {} — {} payload(s)",
            cap.version,
            cap.meta
                .get("captured_utc")
                .map_or("(no date)", String::as_str),
            cap.fixtures.len()
        );
        assert!(
            cap.dir.join("manifest.toml").is_file(),
            "{}: no manifest",
            cap.version
        );
    }
}
