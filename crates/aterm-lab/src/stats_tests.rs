// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A STATS PRINT NAMES EVERY HOME THE LAB GUARDS, NEVER SPELLS IT: under
/// a scratch `HOME` (a fake account home handed in through
/// [`crate::paths::Paths::resolve_with`]) the account's home and the
/// scratch one both become `<home>`, the run's scratch (inside the
/// scratch home here, so the deeper path wins) `<scratch>`.
#[test]
fn the_stats_scrub_names_the_account_home_and_a_scratch_home_both() {
    let top = aterm_tempfile::Builder::new()
        .prefix("lab-stats-")
        .tempdir()
        .unwrap();
    let real = std::fs::canonicalize(top.path()).unwrap();
    let (account, scratch) = (real.join("account"), real.join("scratch"));
    for d in [&account, &scratch] {
        std::fs::create_dir_all(d).unwrap();
    }
    let (a, s) = (account.display().to_string(), scratch.display().to_string());
    let run = Run {
        w: scratch.join("w"),
        sockdir: "/private/tmp/at.sock".into(),
        ..Run::default()
    };
    let text = format!("{a}/.claude.json {s}/.zshrc {s}/w/x /private/tmp/at.sock/proj");
    let p = crate::paths::Paths::resolve_with(Some(account.clone()), Some(scratch.into()));
    assert_eq!(
        scrub_in(&p, &run, &text),
        "<home>/.claude.json <home>/.zshrc <scratch>/x <sockdir>/proj"
    );
    let own = crate::paths::Paths::resolve_with(Some(account.clone()), Some(account.into()));
    assert_eq!(
        scrub_in(&own, &run, &text),
        format!("<home>/.claude.json {s}/.zshrc <scratch>/x <sockdir>/proj")
    );
}

#[test]
fn key_paths_name_every_leaf_and_no_value() {
    let j = J::parse(r#"{"model":{"id":"m","display_name":"M"},"cost":{"total_cost_usd":0.5},"x":[{"y":1}],"e":[]}"#)
        .unwrap();
    let mut out = Vec::new();
    key_paths(&j, "", &mut out);
    assert_eq!(
        out,
        [
            "cost.total_cost_usd",
            "e[]",
            "model.display_name",
            "model.id",
            "x[].y"
        ]
    );
    assert_eq!(at_path(&j, "cost.total_cost_usd"), "0.5");
    assert_eq!(at_path(&j, "model.id"), "m");
    assert_eq!(at_path(&j, "rate_limits.five_hour"), "-");
}

/// Lines of 2.1.301's debug log (labs `uc0` to `uc3`, and the juniper line a
/// five-hour wall's grant read logs).
#[test]
fn the_default_debug_lines_are_those_that_name_a_request() {
    let re = aterm_regex::Regex::new(DEBUG_DEFAULT).unwrap();
    for l in [
        "[DEBUG] fetchUtilization: GET /api/oauth/usage (attempt 1)",
        "[ERROR] Failed to load usage data: proxy refused the connection",
        "[ERROR] Failed to fetch oauth profile from OAuth token: [REDACTED] proxy refused the connection",
        "[DEBUG] [Bootstrap] Fetch failed: ERR_PROXY_TUNNEL",
        "[ERROR] Failed to fetch MCP registry: proxy refused the connection",
        "[DEBUG] $.http.fetch (cc-plugin-telemetry): POST https://api.anthropic.com/api/event_logging/v2/batch",
        "[ERROR] 1P event logging: 141 events failed to export (code=ERR_PROXY_TUNNEL, proxy refused the connection)",
        "[juniper-tide] status fetch failed",
    ] {
        assert!(re.is_match(l), "{l}");
    }
    for l in [
        "[DEBUG] Failed to stat directory <scratch>/home/.claude/agents: ENOENT: no such file or directory",
        "[DEBUG] [engine] turn 1 end (turns=1 usage in=22755 out=12 cost=$0.0456 api=21ms stop=end_turn resultLen=51)",
        "[DEBUG] hooks module cc-plugin-telemetry@builtin telemetry.log settled in 0.1ms (native link, next() included)",
        "[DEBUG] File <scratch>/home/.claude/.claude.json written atomically",
    ] {
        assert!(!re.is_match(l), "{l}");
    }
}

#[test]
fn a_transcript_row_is_its_kind_and_the_text_a_person_typed() {
    let sys = J::parse(r#"{"type":"system","subtype":"local_command","content":"<command-name>/usage</command-name>","isMeta":false}"#).unwrap();
    assert_eq!(
        row_summary(&sys),
        "system/local_command '<command-name>/usage</command-name>'"
    );
    let user = J::parse(r#"{"type":"user","isMeta":true,"message":{"content":"hi"}}"#).unwrap();
    assert_eq!(row_summary(&user), "user meta 'hi'");
    let asst = J::parse(r#"{"type":"assistant","message":{"content":[{"type":"text"}]}}"#).unwrap();
    assert_eq!(row_summary(&asst), "assistant");
}
