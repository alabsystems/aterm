// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the AI CLIs screen (`ai_screen.rs`): projection, Messages entries,
//! shell launch-line quoting, and worker triggers.

use std::path::{Path, PathBuf};

use aterm_agent::ai::config;
use aterm_agent::ai::state::{Mark, State};
use aterm_messages::Intent;

use super::*;

fn cli(id: &'static str, state: State, mark: Mark, fact: &str) -> AiCli {
    let row = connector::connector(id).expect("a connection");
    AiCli {
        id,
        product: row.product,
        claude: row.is_claude(),
        wanted: true,
        state,
        mark,
        fact: fact.to_string(),
        path: None,
        version: Some("1.2.3".to_string()),
        offers_fix: state.needs_act(true),
        mend: None,
    }
}

fn reading() -> AiReading {
    AiReading {
        as_of: 1_000_000,
        offset_s: Some(0),
        preferred: "codex",
        clis: vec![
            cli(
                "claude",
                State::Ok,
                Mark::Ok,
                "signed in (Max) · served 2 h ago",
            ),
            cli("codex", State::SignedOut, Mark::Fail, "no login is stored"),
            cli(
                "agy",
                State::Unproven,
                Mark::Note,
                "its keyring login is unknown (not measured in this build)",
            ),
            AiCli {
                wanted: false,
                ..cli(
                    "muse",
                    State::Off,
                    Mark::Note,
                    "not used on this machine ([ai.muse] use = false)",
                )
            },
            AiCli {
                version: None,
                ..cli(
                    "grok",
                    State::Off,
                    Mark::Note,
                    "[packages] exclude names it",
                )
            },
            AiCli {
                version: None,
                ..cli(
                    "kimi",
                    State::Off,
                    Mark::Note,
                    "uninstalled here on purpose (`aterm pkg install kimi` brings it back)",
                )
            },
        ],
        store_note: None,
        held: None,
    }
}

fn choices(text: &str, fleet: Option<&str>) -> AiChoices {
    let (local, notes) = config::read(text);
    let path = config::FleetSource::Path("/dotfiles/ai-fleet.toml".into());
    let settings = config::resolve(
        &local,
        fleet.map(|_| &path),
        fleet.map(config::read_fleet),
        notes,
    );
    AiChoices::of(&settings)
}

/// Projection renders health summary, per-CLI rows in `VENDORS` order with actions,
/// and provenance for local vs. fleet choices.
#[test]
fn the_projection_says_who_needs_you_and_where_each_choice_came_from() {
    let state = AiState {
        enabled: true,
        looking: false,
        choices: choices(
            "[ai]\nreviewers = [\"codex\", \"claude\"]\nmachine = \"m7\"\n",
            Some("schema = 1\npreferred = \"codex\"\n[machine.m7.muse]\nuse = false\n"),
        ),
        reading: Some(reading()),
        unavailable: None,
    };
    let p = state.projection(true);
    assert!(p.observed && p.enabled && !p.looking);
    assert_eq!(p.health, "Codex CLI needs you \u{b7} looked at 13:46");
    let ids: Vec<&str> = p.clis.iter().map(|row| row.id).collect();
    assert_eq!(ids, connector::ids());
    let row = |id: &str| p.clis.iter().find(|row| row.id == id).expect("a row");
    assert_eq!(row("codex").title, "Codex CLI 1.2.3 \u{b7} preferred");
    assert_eq!(row("codex").detail, "signed-out \u{b7} no login is stored");
    assert_eq!(row("codex").act, Some(AiAct::SignIn));
    assert_eq!(row("claude").title, "Claude Code 1.2.3");
    assert_eq!(row("claude").act, None, "a working login needs nobody");
    assert_eq!(
        row("agy").act,
        None,
        "an unknown keyring login needs no action on a guess"
    );
    assert_eq!(row("muse").act, None);
    assert!(
        row("muse").detail.starts_with("off \u{b7} "),
        "{}",
        row("muse").detail
    );
    assert!(p.preferred_from_fleet);
    assert!(row("muse").use_from_fleet);
    assert!(!row("codex").use_from_fleet);
    let fact = |rows: &[(String, String)], label: &str| {
        rows.iter()
            .find(|(l, _)| l == label)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("no {label} in {rows:?}"))
    };
    assert_eq!(
        fact(&p.review_rows, "Reviewers"),
        "codex, claude, but the one asking (aterm.toml)"
    );
    assert_eq!(
        fact(&p.review_rows, "Permissions"),
        "the asking agent's own; yolo when you ask"
    );
    assert_eq!(
        fact(&p.review_rows, "Each review"),
        "up to 4 reviewers, up to 600 s each, with all their tools"
    );
    assert_eq!(fact(&p.fleet_rows, "Signer"), "none pinned");
    assert_eq!(fact(&p.fleet_rows, "Machine"), "m7");
    assert_eq!(fact(&p.fleet_rows, "Document"), "/dotfiles/ai-fleet.toml");
    assert_eq!(
        fact(&p.fleet_rows, "State"),
        "read; it has a row for this machine"
    );

    let mut missing = state.clone();
    if let Some(reading) = missing.reading.as_mut() {
        reading.clis[1] = cli("codex", State::NotInstalled, Mark::Fail, "not installed");
    }
    let p = missing.projection(true);
    assert_eq!(
        p.clis
            .iter()
            .find(|row| row.id == "codex")
            .and_then(|row| row.act),
        Some(AiAct::Install)
    );

    if let Some(reading) = missing.reading.as_mut() {
        reading.clis[2] = cli("agy", State::NotInstalled, Mark::Fail, "not installed");
        reading.clis[3] = cli("muse", State::SignedOut, Mark::Fail, "no login is stored");
    }
    missing.choices = choices("", None);
    let p = missing.projection(true);
    for (id, action) in [("agy", AiAct::Install), ("muse", AiAct::SignIn)] {
        assert_eq!(
            p.clis
                .iter()
                .find(|row| row.id == id)
                .and_then(|row| row.act),
            Some(action),
            "{id}"
        );
    }

    let mut fine = state.clone();
    if let Some(reading) = fine.reading.as_mut() {
        reading.clis[1] = cli("codex", State::Idle, Mark::Ok, "signed in");
    }
    assert_eq!(
        fine.projection(true).health,
        "No AI CLI needs you \u{b7} looked at 13:46"
    );

    let captured = state.projection(true);
    let failed = AiState {
        unavailable: Some("captured\ncollector\tfailure".to_string()),
        ..state.clone()
    };
    let failed_view = failed.projection(true);
    assert!(failed_view.health.contains("failed"));
    assert!(failed_view.health.contains("captured collector failure"));
    assert!(!failed_view.health.chars().any(char::is_control));
    let refreshing = AiState {
        looking: true,
        ..failed
    }
    .projection(true);
    assert!(refreshing.health.starts_with("Looking"));
    assert!(!refreshing.health.contains("captured collector failure"));
    for later in [&failed_view, &refreshing] {
        assert!(later.health.contains("last observed at 13:46"));
        assert_eq!(
            later.clis, captured.clis,
            "the last observed versions stay available"
        );
        assert_ne!(
            later.health, captured.health,
            "the current look has its own state"
        );
    }

    let off = AiState {
        enabled: false,
        ..state.clone()
    };
    let p = off.projection(true);
    assert!(p.health.starts_with("Off: "), "{}", p.health);
    assert!(
        p.clis.iter().all(|row| row.act.is_none()
            && row.detail == "not looked at: Look for AI CLI problems is off")
    );

    let bare = AiState {
        choices: choices("", None),
        ..state.clone()
    };
    let p = bare.projection(true);
    assert_eq!(
        fact(&p.review_rows, "Reviewers"),
        "every AI CLI used here but the one asking"
    );
    assert_eq!(fact(&p.fleet_rows, "Document"), "none");
    assert!(!p.fleet_from_url);
    let off = AiState {
        choices: choices("[ai]\nreviews = false\n", None),
        ..state.clone()
    };
    assert_eq!(
        fact(&off.projection(true).review_rows, "Reviewers"),
        "none: reviews are off (aterm.toml)"
    );
    assert!(!p.preferred_from_fleet);

    let unread = AiState::default().projection(false);
    assert_eq!(unread.health, "Reading\u{2026}");
}

fn keys(entries: &[Entry]) -> Vec<String> {
    entries.iter().map(Entry::key).collect()
}

/// Wanted CLIs that are signed out, not installed, or due raise `ai.<cli>.<state>`
/// (Warn for preferred, Info otherwise), while accounts v2 handles Claude when populated.
#[test]
fn the_lines_name_each_cli_that_needs_you_and_leave_claude_to_accounts() {
    let entries = ai_entries(&reading(), false);
    assert_eq!(keys(&entries), ["ai.codex.signed-out"]);
    let codex = &entries[0];
    assert_eq!(
        codex.mover().map(Mover::intent),
        Some(&Intent::OpenSettings {
            route: "/ai".to_string()
        })
    );
    let msg = codex.message().expect("the words pass the door");
    assert_eq!(msg.severity, Severity::Warn, "the preferred CLI");
    assert_eq!(msg.key.as_deref(), Some("ai.codex.signed-out"));
    assert!(
        msg.detail
            .iter()
            .any(|l| l.contains("Sign in to Codex CLI in Settings")),
        "{:?}",
        msg.detail
    );
    assert!(
        msg.detail.iter().any(|l| l.contains("aterm ai doctor")),
        "{:?}",
        msg.detail
    );

    let mut r = reading();
    r.preferred = "claude";
    for (state, mark, key, step) in [
        (
            State::SignedOut,
            Mark::Fail,
            "ai.codex.signed-out",
            "Sign in to Codex CLI in Settings",
        ),
        (
            State::NotInstalled,
            Mark::Fail,
            "ai.codex.not-installed",
            "Install Codex CLI in Settings",
        ),
        (
            State::Due(2_000_000),
            Mark::Warn,
            "ai.codex.due",
            "Sign in to Codex CLI in Settings",
        ),
    ] {
        r.clis[1] = cli("codex", state, mark, "what the look saw");
        let entries = ai_entries(&r, false);
        assert_eq!(keys(&entries), [key]);
        let msg = entries[0].message().expect("the words pass the door");
        assert_eq!(msg.severity, Severity::Info, "{key}");
        assert!(
            msg.detail.iter().any(|l| l.contains(step)),
            "{key}: {:?}",
            msg.detail
        );
    }
    for (state, mark) in [
        (State::Ok, Mark::Ok),
        (State::Idle, Mark::Ok),
        (State::Limited(2_000_000), Mark::Note),
        (State::Unproven, Mark::Note),
        (State::Off, Mark::Note),
    ] {
        r.clis[1] = cli("codex", state, mark, "fine");
        assert!(ai_entries(&r, false).is_empty(), "{state:?}");
    }
    r.clis[1] = cli("codex", State::Ok, Mark::Ok, "fine");
    r.clis[2] = cli("agy", State::NotInstalled, Mark::Fail, "not installed");
    assert_eq!(keys(&ai_entries(&r, false)), ["ai.agy.not-installed"]);
    r.clis[2] = cli("agy", State::Unproven, Mark::Note, "keyring unknown");
    assert!(ai_entries(&r, false).is_empty(), "agy's login is unknown");
    r.clis[2] = cli("agy", State::Ok, Mark::Ok, "fine");
    r.clis[1] = AiCli {
        wanted: false,
        ..cli("codex", State::SignedOut, Mark::Fail, "no login")
    };
    assert!(ai_entries(&r, false).is_empty(), "codex is not wanted here");

    let mut r = reading();
    r.clis[1] = cli("codex", State::Idle, Mark::Ok, "signed in");
    for state in [State::SignedOut, State::NotInstalled] {
        r.clis[0] = cli("claude", state, Mark::Fail, "no login");
        assert_eq!(
            keys(&ai_entries(&r, false)),
            [format!("ai.claude.{}", state.word())]
        );
        assert!(
            ai_entries(&r, true).is_empty(),
            "{state:?}: accounts speaks for Claude"
        );
    }
    r.clis[0] = cli("claude", State::Due(2_000_000), Mark::Warn, "renew by …");
    assert!(
        ai_entries(&r, false).is_empty(),
        "a due Claude login is accounts v2's"
    );
}

/// A CODEX ROW THAT OFFERS NO FIX ASKS NOTHING OF ANYONE: the window's reading carries whether
/// each row offers a fix ([`AiReading::of`]), and a signed-out or due Codex row that offers none
/// (nothing here signs it in: another Mac's warm set holds it, or its login still works and only
/// Codex's own sign-in renews it) raises no entry, offers no Sign in and is not said to need you,
/// so no press opens a tab whose `aterm ai login codex` refuses. CONTROL: the same rows with
/// their fix raise their entry and offer Sign in.
#[test]
fn a_codex_row_that_offers_no_fix_asks_nothing_of_anyone() {
    let codex = connector::connector("codex").expect("a connection");
    let (local, notes) = config::read("[ai]\npreferred = \"codex\"\n");
    let settings = config::resolve(&local, None, None, notes);
    for state in [State::SignedOut, State::Due(2_000_000)] {
        for fix in [None, Some("aterm ai login codex".to_string())] {
            let offered = fix.is_some();
            let look = aterm_agent::ai::cli::Look {
                settings: settings.clone(),
                facts: Vec::new(),
                rows: vec![aterm_agent::ai::state::Row {
                    connector: codex,
                    wanted: true,
                    state,
                    mark: Mark::Fail,
                    fact: "what the look saw".to_string(),
                    actor: offered.then_some(aterm_agent::ai::state::Actor::Person),
                    fix,
                    mend: None,
                    version: None,
                    plan: None,
                    used: None,
                    renew_by: None,
                    limited_until: None,
                }],
                store_note: None,
                asker: aterm_agent::ai::permission::Asker::Person,
                held: None,
            };
            let reading = AiReading::of(&look, 1_000_000, Some(0));
            let case = format!("{state:?}, offered {offered}");
            assert_eq!(
                keys(&ai_entries(&reading, false)),
                if offered {
                    vec![format!("ai.codex.{}", state.word())]
                } else {
                    Vec::new()
                },
                "{case}"
            );
            let ai = AiState {
                enabled: true,
                looking: false,
                choices: AiChoices::of(&settings),
                reading: Some(reading),
                unavailable: None,
            };
            let sign_in = offered.then_some(AiAct::SignIn);
            assert_eq!(
                ai.actionable_cli("codex").and_then(AiCli::act),
                sign_in,
                "{case}"
            );
            let p = ai.projection(true);
            let row = p.clis.iter().find(|row| row.id == "codex").expect("a row");
            assert_eq!(row.act, sign_in, "{case}");
            assert_eq!(
                p.health.starts_with(codex.product),
                offered,
                "{case}: {}",
                p.health
            );
        }
    }
}

/// A CLAUDE ROW WHOSE SIGN-IN STARTS NOTHING OFFERS NO SIGN IN AND STILL NEEDS YOU: where
/// accounts.toml is refused, `aterm ai login claude` only says so, so the row the agent's look
/// judges ([`aterm_agent::ai::state::assess`]) offers no command but a person's step
/// ([`aterm_agent::ai::state::Row::mend`]), and the window raises no entry and offers no Sign in,
/// while the card's first line says Claude Code needs you, the row's fact says why and its detail
/// names the step. CONTROL: the same row with no roster, whose sign-in asks a person for the
/// account's email, raises its entry and offers Sign in, and names no step of its own.
#[test]
fn a_claude_row_whose_sign_in_starts_nothing_offers_no_sign_in() {
    use aterm_agent::ai::facts::{ClaudeAccounts, Facts, Managed};
    let claude = connector::connector("claude").expect("a connection");
    let (local, notes) = config::read("[ai]\npreferred = \"claude\"\n");
    let settings = config::resolve(&local, None, None, notes);
    let view = |refused: Option<&str>| ClaudeAccounts {
        label: "main".to_string(),
        listed: false,
        refused: refused.map(str::to_string),
        unread: None,
        renews: false,
        login: None,
        made: false,
        state: None,
        renew_by: None,
        limited_until: None,
        served_at: None,
        renewed_at: None,
        signed_out_at: None,
        next_sign_in: None,
        tried: None,
        unattended: false,
        off: None,
    };
    for refused in [Some("row 1: no label"), None] {
        let offered = refused.is_none();
        let facts = Facts {
            connector: claude,
            kept: None,
            managed: Some(Managed {
                path: PathBuf::from("/pkg/agents/claude"),
                version: None,
            }),
            foreign: None,
            home: Ok(PathBuf::from("/home/me/.claude")),
            home_var: None,
            home_exists: false,
            login_file: None,
            hidden_store: false,
            profile_signed_in: false,
            plan: None,
            accounts: Some(view(refused)),
            limited_until: None,
            record: aterm_agent::ai::store::Record::default(),
            env_credentials: Vec::new(),
            pool: None,
        };
        let row = aterm_agent::ai::state::assess(&facts, true, 1_000_000);
        assert_eq!(row.state, State::SignedOut, "{}", row.fact);
        let look = aterm_agent::ai::cli::Look {
            settings: settings.clone(),
            facts: vec![facts],
            rows: vec![row],
            store_note: None,
            asker: aterm_agent::ai::permission::Asker::Person,
            held: None,
        };
        let reading = AiReading::of(&look, 1_000_000, Some(0));
        let case = format!("refused {refused:?}");
        let cli = reading.cli("claude").expect("claude");
        assert_eq!(cli.offers_fix, offered, "{case}");
        assert_eq!(
            cli.fact.contains("accounts.toml is refused"),
            !offered,
            "{case}: {}",
            cli.fact
        );
        assert_eq!(
            keys(&ai_entries(&reading, false)),
            if offered {
                vec!["ai.claude.signed-out".to_string()]
            } else {
                Vec::new()
            },
            "{case}"
        );
        let ai = AiState {
            enabled: true,
            looking: false,
            choices: AiChoices::of(&settings),
            reading: Some(reading),
            unavailable: None,
        };
        let sign_in = offered.then_some(AiAct::SignIn);
        assert_eq!(
            ai.actionable_cli("claude").and_then(AiCli::act),
            sign_in,
            "{case}"
        );
        let p = ai.projection(true);
        let row = p.clis.iter().find(|row| row.id == "claude").expect("a row");
        assert_eq!(row.act, sign_in, "{case}");
        assert!(
            p.health.starts_with("Claude Code needs you"),
            "{case}: {}",
            p.health
        );
        let step = "mend accounts.toml (`aterm harness accounts` says how), then sign claude in";
        assert_eq!(
            row.detail.ends_with(step),
            !offered,
            "{case}: {}",
            row.detail
        );
    }
}

/// Shell launch lines quote the executable path and refuse control characters or unsafe expansions.
#[test]
fn the_sign_in_line_quotes_the_program_and_refuses_a_control_character() {
    assert_eq!(
        launch_line(
            Path::new("/Applications/aterm.app/Contents/MacOS/aterm"),
            &["ai", "login", "codex"]
        )
        .as_deref(),
        Some("'/Applications/aterm.app/Contents/MacOS/aterm' ai login codex")
    );
    assert_eq!(
        launch_line(Path::new("/tmp/it's/aterm"), &["pkg", "install", "codex"]).as_deref(),
        Some("'/tmp/it'\\''s/aterm' pkg install codex")
    );

    let args = &["ai", "login", "codex"];
    let exe = Path::new(r"C:\Program Files\aterm.exe");
    for shell in [r"C:\Windows\System32\cmd.exe", "CMD.EXE"] {
        assert_eq!(
            windows_launch_line(exe, args, Path::new(shell)).as_deref(),
            Some(r#""C:\Program Files\aterm.exe" ai login codex"#)
        );
    }
    for shell in [r"C:\Program Files\PowerShell\7\pwsh.exe", "powershell"] {
        assert_eq!(
            windows_launch_line(exe, args, Path::new(shell)).as_deref(),
            Some(r"& 'C:\Program Files\aterm.exe' ai login codex")
        );
        assert_eq!(
            windows_launch_line(
                Path::new("C:\\it's\\it\u{2019}s\\aterm.exe"),
                args,
                Path::new(shell)
            )
            .as_deref(),
            Some("& 'C:\\it''s\\it\u{2019}\u{2019}s\\aterm.exe' ai login codex")
        );
    }
    let metachars = Path::new(r"C:\A&B (dev);$x\aterm.exe");
    assert_eq!(
        windows_launch_line(metachars, args, Path::new("cmd")).as_deref(),
        Some(r#""C:\A&B (dev);$x\aterm.exe" ai login codex"#)
    );
    assert_eq!(
        windows_launch_line(metachars, args, Path::new("pwsh")).as_deref(),
        Some(r"& 'C:\A&B (dev);$x\aterm.exe' ai login codex")
    );
    for path in [r"C:\%AT_USER%\aterm.exe", r"C:\!AT_USER!\aterm.exe"] {
        assert!(windows_launch_line(Path::new(path), args, Path::new("cmd")).is_none());
        assert!(windows_launch_line(Path::new(path), args, Path::new("pwsh")).is_some());
    }
    for shell in ["wsl.exe", "bash.exe", "nu.exe", ""] {
        assert!(windows_launch_line(exe, args, Path::new(shell)).is_none());
    }
    for path in ["C:\\bad\nname\\aterm.exe", "C:\\bad\"name\\aterm.exe"] {
        for shell in ["cmd", "pwsh"] {
            assert!(windows_launch_line(Path::new(path), args, Path::new(shell)).is_none());
        }
    }
    assert_eq!(
        launch_line(
            &PathBuf::from("/tmp/a\nrm -rf ~/aterm"),
            &["ai", "login", "codex"]
        ),
        None
    );
}

/// The worker looks on start when enabled, on explicit ask, and when the wake marker changes.
#[test]
fn the_worker_looks_at_start_on_ask_and_on_a_rewritten_wake() {
    let dir = aterm_tempfile::tempdir().expect("scratch folder");
    let wake = dir.path().join("wake");
    let (asks, asked) = std::sync::mpsc::channel();
    let (looked, looks) = std::sync::mpsc::channel::<u64>();
    let worker = {
        let wake = wake.clone();
        std::thread::spawn(move || {
            let look = |_| Ok(reading());
            let send = |generation, _| looked.send(generation).is_ok();
            look_loop(
                &asked,
                true,
                0,
                Some(&wake),
                (LOOK_EVERY, Duration::from_millis(20)),
                &look,
                &send,
            );
        })
    };
    let must = Duration::from_mins(1);
    assert_eq!(looks.recv_timeout(must).expect("a look at start"), 0);
    asks.send(AiAsk::Look(1)).expect("the worker runs");
    assert_eq!(looks.recv_timeout(must).expect("a look when asked"), 1);
    std::fs::write(&wake, "").expect("the wake marker");
    assert_eq!(
        looks
            .recv_timeout(must)
            .expect("a look when the wake marker appears"),
        1,
        "an automatic look retains the admitted generation"
    );
    asks.send(AiAsk::Off).expect("the worker runs");
    asks.send(AiAsk::Look(2)).expect("the worker runs");
    assert_eq!(looks.recv_timeout(must).expect("a look once on again"), 2);
    drop(asks);
    worker.join().expect("the worker ends with its asker");
}

#[test]
fn a_policy_change_retires_the_previous_ai_reading() {
    let mut app = App::headless_for_test();
    app.finish_ai_look(0, Ok(reading()));
    assert!(
        app.ai_state().reading.is_some(),
        "the admitted reading is visible"
    );
    assert!(
        app.ai_state()
            .projection(true)
            .clis
            .iter()
            .any(|row| row.act.is_some())
    );

    app.config.ai = crate::app_config::AiPolicy::read("[ai.codex]\nuse = false\n");
    app.ai_config_changed();
    assert!(app.config.ai.enabled(), "only this CLI's use changed");
    assert!(
        app.ai_state().reading.is_none(),
        "old policy facts must retire before the replacement look"
    );
    assert!(
        app.ai_state()
            .projection(true)
            .clis
            .iter()
            .all(|row| row.act.is_none())
    );
}

/// `[ai]` turned off, or its policy changed, ends the AI entries withdrawn: no look
/// read their causes end, so none is filed as fixed. A reporter that read the cause
/// end (the negative control: a clear) files the same entry fixed.
#[test]
fn turning_ai_off_withdraws_its_entries_and_claims_no_fix() {
    use aterm_messages::{Outcome, Retired};
    const KEY: &str = "ai.codex.signed-out";
    let open = |app: &App| app.messages.digest().entries.iter().any(|e| e.key == KEY);
    let ended = |app: &App| {
        app.messages
            .log()
            .records()
            .rev()
            .find(|r| r.key.as_deref() == Some(KEY))
            .and_then(|r| r.retired().cloned())
    };
    let raised = || {
        let mut app = App::headless_for_test();
        app.finish_ai_look(0, Ok(reading()));
        assert!(open(&app), "the preferred signed-out CLI raises its entry");
        app
    };
    for policy in ["[ai]\nenabled = false\n", "[ai.codex]\nuse = false\n"] {
        let mut app = raised();
        app.config.ai = crate::app_config::AiPolicy::read(policy);
        app.ai_config_changed();
        assert!(!open(&app), "{policy:?}");
        assert_eq!(ended(&app), Some(Retired::Withdrawn), "{policy:?}");
    }
    let mut app = raised();
    app.except_clear(KEY);
    assert!(!open(&app));
    assert_eq!(ended(&app), Some(Retired::Resolved(Outcome::Ok)));
}

#[test]
fn a_current_use_refusal_removes_an_old_readings_install_action() {
    let mut old = reading();
    old.clis[1] = cli("codex", State::NotInstalled, Mark::Fail, "not installed");
    let mut state = AiState {
        enabled: true,
        choices: choices("[ai.codex]\nuse = true\n", None),
        reading: Some(old),
        ..AiState::default()
    };
    let act = |state: &AiState| {
        state
            .projection(true)
            .clis
            .into_iter()
            .find(|row| row.id == "codex")
            .unwrap()
            .act
    };
    assert_eq!(act(&state), Some(AiAct::Install), "ordinary wanted install");
    state.choices = choices("[ai.codex]\nuse = false\n", None);
    assert_eq!(
        act(&state),
        None,
        "the current policy wins over a stale reading"
    );
}

#[test]
fn an_old_ai_completion_cannot_settle_a_new_policy_look() {
    let mut app = App::headless_for_test();
    let (asks, asked) = std::sync::mpsc::channel();
    app.ai_screen.asks = Some(asks);
    app.ask_ai_look();
    let AiAsk::Look(old) = asked.try_recv().expect("first request") else {
        panic!("look")
    };
    app.config.ai = crate::app_config::AiPolicy::read("[ai.codex]\nuse = false\n");
    app.ai_config_changed();
    let AiAsk::Look(current) = asked.try_recv().expect("new policy request") else {
        panic!("look")
    };
    assert_ne!(old, current);

    app.finish_ai_look(old, Ok(reading()));
    assert!(app.ai_state().reading.is_none(), "old facts cannot return");
    assert!(
        app.ai_state().looking,
        "the replacement still owns its completion"
    );
    app.finish_ai_look(old, Err("old failure".into()));
    assert!(app.ai_state().unavailable.is_none());
    let mut admitted = reading();
    admitted.clis[1].wanted = false;
    app.finish_ai_look(current, Ok(admitted.clone()));
    assert_eq!(app.ai_state().reading, Some(admitted));
    assert!(
        !app.ai_state().looking,
        "the current completion settles the look"
    );

    app.config.ai = crate::app_config::AiPolicy::read("[ai]\nenabled = false\n");
    app.ai_config_changed();
    assert_eq!(asked.try_recv().expect("off request"), AiAsk::Off);
    app.config.ai = crate::app_config::AiPolicy::read("");
    app.ai_config_changed();
    let AiAsk::Look(reenabled) = asked.try_recv().expect("on request") else {
        panic!("look")
    };
    app.finish_ai_look(current, Ok(reading()));
    assert!(
        app.ai_state().reading.is_none(),
        "off then on cannot admit a pre-off result"
    );
    app.finish_ai_look(reenabled, Ok(reading()));
    assert!(app.ai_state().reading.is_some());
}

/// Extracts the full path named by a fact, including spaces, or `None` when truncated.
#[test]
fn a_reading_names_the_whole_path_its_fact_says() {
    let path = Path::new("/Users//someone/Library/Application Support/aterm/pkg/reroute/agy");
    let fact = format!(
        "not installed by aterm (a copy it does not manage is at {}) (not measured in this build)",
        path.display()
    );
    assert_eq!(
        named_path(&fact, Some(path)).as_deref(),
        Some("/Users//someone/Library/Application Support/aterm/pkg/reroute/agy")
    );
    assert_eq!(named_path(&fact, None), None);
    assert_eq!(named_path("not installed", Some(path)), None);
    assert_eq!(
        named_path(
            "not installed by aterm (a copy it does not manage is at /Users//someone/Library/Appl",
            Some(path)
        ),
        None
    );
}
