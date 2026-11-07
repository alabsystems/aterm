// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::restore::AgentRestore;
use aterm_agent::harness::relaunch::{AgentRun, Snapshot};

fn original(case: i64) -> Snapshot {
    let profile = (case > 2).then(|| Context {
        plain: "/fixture/.claude.json".into(),
        records: "/fixture/.claude".into(),
        selection: match case {
            5 => Selection::CustomOauth,
            6 => Selection::Unknown,
            _ => Selection::Standard,
        },
        home: (case > 3).then(|| if case == 4 { "" } else { "/fixture" }.into()),
        config: (case == 4).then(String::new),
        login: (case > 3).then(|| if case == 4 { "" } else { "/fixture/login" }.into()),
    });
    let route = if case != 2 && case != 5 && case != 0 {
        AuthRoute::Stored
    } else {
        AuthRoute::Outside
    };
    // Case 7 is a stored login an older build captured, with no launch record.
    let launch_env = aterm_agent::harness::accounts::LaunchEnv::of_exec_env(&[
        "HOME=/fixture".into(),
        "PATH=/usr/bin".into(),
        format!(
            "{}=/fixture/login",
            aterm_agent::harness::accounts::LOGIN_ENV
        ),
    ])
    .filter(|_| route == AuthRoute::Stored && case != 7);
    Snapshot {
        launch_env,
        tab: "s-restore-auth".into(),
        pid: 42,
        start: "Thu Nov 05 01:00:00 2026".into(),
        shell: 41,
        program: "/fixture/claude".into(),
        argv: vec!["/fixture/claude".into()],
        session: Some("0badf00d-1111-2222-3333-444455556666".into()),
        cwd: "/fixture/work".into(),
        version: None,
        run: AgentRun::Claude,
        dialect: None,
        login: Some("/fixture/login".into()),
        home: None,
        claude_dir: None,
        route,
        auth_observed: case != 0,
        profile,
    }
}

fn restore_case(case: i64) -> [i64; 3] {
    let before = original(case);
    let mut record = AgentRestore::of(before.clone());
    if case == 8 {
        record.word = "codex".into();
    }
    let json = aterm_json::to_vec(&record).unwrap();
    let toml = aterm_toml::to_string(&record).unwrap();
    let reads = [
        aterm_json::from_slice::<AgentRestore>(&json).ok(),
        aterm_toml::from_str::<AgentRestore>(&toml).ok(),
    ];
    let mut preserved = true;
    let mut synthesized = false;
    let mut accepted = true;
    for read in reads {
        // A mixed-provider record must fail decoding itself. A later refusal to
        // construct a snapshot must not conceal a decoder that admitted it.
        let decoded = read.is_some();
        let restored = read.and_then(|record| record.snapshot(before.tab.clone(), before.shell));
        accepted &= decoded && (case == 8 || restored.is_some());
        if let Some(after) = restored {
            preserved &= before.route == after.route
                && before.auth_observed == after.auth_observed
                && before.profile == after.profile
                && before.login == after.login
                && before.home == after.home
                && before.launch_env == after.launch_env;
            if case == 0 {
                synthesized |= AgentRestore::of(after).claude_auth.is_some();
            }
        }
    }
    [
        i64::from(preserved),
        i64::from(synthesized),
        i64::from(accepted),
    ]
}

pub(crate) fn restore_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    if action == "NextCase" {
        next.insert("case", state["case"] + 1);
    } else {
        let [preserved, synthesized, accepted] = restore_case(state["case"]);
        next.insert("preserved", preserved);
        next.insert("synthesized", synthesized);
        next.insert("accepted", accepted);
        next.insert("phase", 1);
    }
    next
}

#[test]
fn claude_restore_auth_conforms_without_inventing_or_dropping_attribution() {
    let model = aterm_models_harness::claude_restore_auth_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = restore_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
            }
        }
    }
    let mut bad = model.init_state();
    bad = restore_transition(&bad, "Restore");
    bad.insert("synthesized", 1);
    assert!(!model.check_invariant("NoInventedEvidence", &bad));
    bad.insert("preserved", 0);
    assert!(!model.check_invariant("CapturedAttribution", &bad));
}

/// One Claude Code agent's restore record in a layout, as `TOML` words.
fn record_of(auth: &str) -> AgentRestore {
    let text = format!(
        "word = \"claude\"\npid = 42\nstart = \"Thu Nov 05 01:00:00 2026\"\n\
         program = \"/fixture/claude\"\nargv = [\"claude\"]\ncwd = \"/fixture/work\"\n\
         \n[claude_auth]\n{auth}\n"
    );
    aterm_toml::from_str(&text).unwrap_or_else(|e| panic!("{text}: {e}"))
}

#[test]
fn an_older_builds_stored_login_reads_as_its_route_and_a_new_record_never_writes_it() {
    for (legacy, route) in [
        ("stored_login = true", AuthRoute::Stored),
        ("stored_login = false", AuthRoute::Outside),
    ] {
        let record = record_of(legacy);
        let auth = record.claude_auth.clone().expect("the record's auth");
        assert_eq!(auth.route(), route, "{legacy}");
        // Read back and written as it is, an older record keeps what it said.
        let kept = aterm_toml::to_string(&auth).unwrap();
        assert!(kept.contains(legacy), "{kept}");
        let kept: ClaudeAuthRestore = aterm_toml::from_str(&kept).unwrap();
        assert_eq!(kept.route(), route, "{legacy}");
        let snap = record
            .clone()
            .snapshot("s-restore-auth".into(), 41)
            .expect("a Claude Code record relaunches");
        assert_eq!(snap.route, route, "{legacy}");
        assert!(snap.auth_observed);
        // Written again, the record names its route and no longer the legacy bit.
        let written = aterm_toml::to_string(&AgentRestore::of(snap)).unwrap();
        assert!(!written.contains("stored_login"), "{written}");
        assert!(written.contains("route = "), "{written}");
        let reread: AgentRestore = aterm_toml::from_str(&written).unwrap();
        assert_eq!(reread.claude_auth.map(|auth| auth.route()), Some(route));
    }
    // Negative control: a record with neither word reads as outside, never as a stored login.
    assert_eq!(
        record_of("").claude_auth.map(|auth| auth.route()),
        Some(AuthRoute::Outside)
    );
}

#[test]
fn a_record_with_its_route_reads_it_over_any_legacy_bit() {
    for (words, route) in [
        ("route = \"stored\"", AuthRoute::Stored),
        ("route = \"outside\"", AuthRoute::Outside),
        (
            "route = \"outside\"\nstored_login = true",
            AuthRoute::Outside,
        ),
        (
            "route = \"stored\"\nstored_login = false",
            AuthRoute::Stored,
        ),
    ] {
        assert_eq!(
            record_of(words).claude_auth.map(|auth| auth.route()),
            Some(route),
            "{words}"
        );
    }
    let entry = TokenEntry::parse("aterm-claude-token-ac-0123456789abcdef").unwrap();
    let token = AuthRoute::VaultToken { entry };
    let record = ClaudeAuthRestore::new(token, None, None);
    for text in [
        aterm_toml::to_string(&record).unwrap(),
        aterm_json::to_string(&record).unwrap(),
    ] {
        assert!(
            text.contains("aterm-claude-token-ac-0123456789abcdef"),
            "{text}"
        );
        let read: ClaudeAuthRestore = aterm_toml::from_str(&text)
            .or_else(|_| aterm_json::from_str(&text))
            .unwrap_or_else(|e| panic!("{text}: {e}"));
        assert_eq!(read.route(), token);
    }
    // A token route's record names only a token entry: its record (`-meta`) is no entry.
    assert!(
        aterm_toml::from_str::<ClaudeAuthRestore>(
            "[route.token]\nentry = \"aterm-claude-token-ac-0123456789abcdef-meta\"\n"
        )
        .is_err()
    );
}

/// Each `claude_auth` table a shipped producer's layout holds, as its own `TOML` words.
fn shipped_claude_auths(layout: &str) -> Vec<String> {
    let mut tables = Vec::new();
    let mut open: Option<String> = None;
    for line in layout.lines() {
        if line.starts_with('[') {
            tables.extend(open.take());
            if line.ends_with(".claude_auth]") {
                open = Some(String::new());
            }
        } else if let Some(table) = open.as_mut() {
            table.push_str(line);
            table.push('\n');
        }
    }
    tables.extend(open);
    tables
}

#[test]
fn every_shipped_producers_claude_auth_reads_as_the_route_it_recorded() {
    let root = aterm_tempfile::manifest_dir().join("tests/fixtures/handoff");
    let mut read = 0;
    let mut stored_at_155 = 0;
    for release in std::fs::read_dir(&root).unwrap() {
        let release = release.unwrap().path();
        let Ok(carried) = std::fs::read_dir(release.join("carried-state")) else {
            continue;
        };
        for file in carried {
            let file = file.unwrap().path();
            if !file.to_string_lossy().ends_with(".layout.toml") {
                continue;
            }
            for table in shipped_claude_auths(&std::fs::read_to_string(&file).unwrap()) {
                let auth: ClaudeAuthRestore = aterm_toml::from_str(&table)
                    .unwrap_or_else(|e| panic!("{}: {table}: {e}", file.display()));
                let recorded = if table.contains("stored_login = true") {
                    AuthRoute::Stored
                } else {
                    AuthRoute::Outside
                };
                assert_eq!(auth.route(), recorded, "{}: {table}", file.display());
                read += 1;
                if release.ends_with("v0.155.0") && recorded == AuthRoute::Stored {
                    stored_at_155 += 1;
                }
            }
        }
    }
    assert!(read > 0, "the shipped producers record Claude Code's auth");
    assert!(
        stored_at_155 > 0,
        "v0.155.0's desk records a stored login as `stored_login = true`"
    );
}
