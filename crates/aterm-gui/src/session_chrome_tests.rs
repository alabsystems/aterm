// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn cached_at(composed_ms: u64) -> CachedChrome {
    CachedChrome {
        high_id: 0,
        label: "session".to_string(),
        can_rename: true,
        activity_revision: 0,
        connections_revision: 0,
        upgrade_revision: 0,
        goal: 0,
        expires_ms: Some(composed_ms + CACHE_MAX_AGE_MS),
        clause: None,
        clause_line: None,
        ext: TabChromeExt::default(),
    }
}

#[test]
fn expiry_batch_is_deadline_ordered_bounded_and_self_disarming() {
    let mut cache = std::collections::HashMap::new();
    cache.insert(9, cached_at(20));
    cache.insert(7, cached_at(10));
    cache.insert(5, cached_at(10));
    assert_eq!(next_cache_expiry_ms(&cache), Some(10 + CACHE_MAX_AGE_MS));

    let due_at_boundary = due_cache_batch(&cache, 10 + CACHE_MAX_AGE_MS, 1);
    assert_eq!(
        due_at_boundary,
        vec![5],
        "equal deadlines use session id order"
    );
    let due = due_cache_batch(&cache, 20 + CACHE_MAX_AGE_MS, 2);
    assert_eq!(due, vec![5, 7], "one turn never exceeds its fixed budget");

    for session in due {
        cache.remove(&session);
    }
    assert_eq!(
        due_cache_batch(&cache, 20 + CACHE_MAX_AGE_MS, 2),
        vec![9],
        "the finite remainder stays immediately due for the next turn"
    );
    cache.clear();
    assert_eq!(
        next_cache_expiry_ms(&cache),
        None,
        "empty cache owns no wake"
    );
}

const NOW: u64 = 1_000_000_000;

#[test]
fn the_clock_moves_a_composition_only_by_an_upgrades_words() {
    let mut input = full_input();
    assert_eq!(clock_expiry_ms(NOW, &input), None, "its ages arm nothing");
    let menu = compose_tab_menu(&input);
    let aged = |at: u64| -> Vec<String> {
        menu_shown_at(&menu, at)
            .into_iter()
            .filter_map(|e| match e {
                TabMenuEntry::Header(h) if h.contains(" \u{b7} ") => Some(h),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        aged(NOW),
        [
            "details changed \u{b7} just now",
            "changed directory \u{b7} 2m ago",
            "started \u{b7} 2h ago"
        ]
    );
    assert_eq!(
        aged(NOW + 60_000),
        [
            "details changed \u{b7} 1m ago",
            "changed directory \u{b7} 3m ago",
            "started \u{b7} 2h ago"
        ],
        "the same composition, aged a minute on"
    );
    let line = tab_menu_chrome_line(0, &menu, NOW + 60_000);
    assert!(
        line.contains("\"changed directory \u{b7} 3m ago\""),
        "{line}"
    );
    input.upgrade = Some(UpgradeMenu {
        sid: "s-1".to_string(),
        moving: "Claude Code 2.1.281 → 2.1.282".to_string(),
        to: "2.1.282".to_string(),
        words: Vec::new(),
    });
    assert_eq!(clock_expiry_ms(NOW, &input), Some(NOW + CACHE_MAX_AGE_MS));
    let mut cache = std::collections::HashMap::new();
    let mut quiet = cached_at(NOW);
    quiet.expires_ms = None;
    cache.insert(3, quiet);
    assert_eq!(next_cache_expiry_ms(&cache), None);
    assert!(due_cache_batch(&cache, u64::MAX, 4).is_empty());
}

fn full_input() -> SessionChromeInput {
    SessionChromeInput {
        can_rename: true,
        label: "build agent".to_string(),
        icon: Some("🤖".to_string()),
        description: Some("Rebuilds the docs site".to_string()),
        activity: Some("Building documentation".to_string()),
        goal: None,
        cwd: Some("/Users//foo/src/aterm".to_string()),
        home: Some("/Users//foo".to_string()),
        state: Some("alive".to_string()),
        has_session: true,
        timeline: vec![
            TimelineNote {
                kind: "meta-change",
                at_ms: NOW - 3_000,
            },
            TimelineNote {
                kind: "cwd-change",
                at_ms: NOW - 125_000,
            },
            TimelineNote {
                kind: "spawned",
                at_ms: NOW - 7_200_000,
            },
        ],
        connections: Vec::new(),
        upgrade: None,
        attention: None,
        sid: None,
        agent: AgentMenu::default(),
        clause: None,
        rewind: false,
    }
}

fn fact(
    sid: &str,
    title: &str,
    direction: ConnDirection,
    kind: ConnFlow,
    live: bool,
) -> ConnectionFact {
    ConnectionFact {
        peer_sid: SessionId::new(sid),
        peer_title: title.to_string(),
        direction,
        kind,
        live,
    }
}

#[test]
fn tooltip_orders_identity_with_home_abbreviation() {
    let tip = compose_tooltip(&full_input()).expect("full metadata composes");
    assert_eq!(
        tip,
        "🤖 build agent\n\
         description: Rebuilds the docs site\n\
         activity: Building documentation\n\
         cwd: ~/src/aterm"
    );
    let mut exited = full_input();
    exited.state = Some("exited".to_string());
    assert!(
        compose_tooltip(&exited)
            .unwrap()
            .ends_with("\ncwd: ~/src/aterm\nstate: exited")
    );
    exited.activity = Some("exited (exit 1)".to_string());
    let tip = compose_tooltip(&exited).unwrap();
    assert!(tip.contains("activity: exited (exit 1)"), "{tip}");
    assert!(!tip.contains("state: exited"), "{tip}");
    let bare = SessionChromeInput {
        label: "zsh".to_string(),
        state: Some("alive".to_string()),
        has_session: true,
        timeline: vec![TimelineNote {
            kind: "spawned",
            at_ms: NOW - 7_200_000,
        }],
        ..SessionChromeInput::default()
    };
    assert_eq!(compose_tooltip(&bare), None);
}

#[test]
fn the_agents_active_goal_is_a_line_of_the_hover_and_the_tab_menu() {
    let mut input = full_input();
    input.goal = Some("implement the aos aterm vision".to_string());
    let tip = compose_tooltip(&input).expect("composes");
    assert_eq!(
        tip,
        "🤖 build agent\n\
         description: Rebuilds the docs site\n\
         activity: Building documentation\n\
         goal: implement the aos aterm vision\n\
         cwd: ~/src/aterm"
    );
    let menu = compose_tab_menu(&input);
    assert!(
        menu.contains(&TabMenuEntry::Header(
            "goal: implement the aos aterm vision".to_string()
        )),
        "{menu:?}"
    );
    input.goal = Some(format!("spoof\u{202e}ed {}", "word ".repeat(80)));
    let tip = compose_tooltip(&input).expect("composes");
    let line = tip
        .lines()
        .find(|l| l.starts_with(GOAL_PREFIX))
        .expect("goal line");
    assert!(line.starts_with("goal: spoofed word"), "{line}");
    assert!(
        line.ends_with('…'),
        "a long goal is cut for display: {line}"
    );
    assert!(!crate::session_timeline::metadata_has_forbidden_formatting(
        line
    ));
    let bare = SessionChromeInput {
        label: "claude".to_string(),
        goal: Some("ship it".to_string()),
        ..SessionChromeInput::default()
    };
    assert_eq!(
        compose_tooltip(&bare).as_deref(),
        Some("claude\ngoal: ship it"),
        "a goal alone is worth a hover"
    );
}

#[test]
fn chrome_defensively_strips_controls_and_bidi_from_legacy_input() {
    let input = SessionChromeInput {
        label: "raw\u{202e}title".to_string(),
        icon: Some("\u{2066}🚀\u{2069}".to_string()),
        description: Some("first\nsecond\u{2029}third".to_string()),
        activity: Some("running\u{0085}tests".to_string()),
        cwd: Some("/tmp/first\nsecond\u{202e}spoof".to_string()),
        ..SessionChromeInput::default()
    };
    let tooltip = compose_tooltip(&input).expect("sanitized fields still compose");
    assert_eq!(
        tooltip,
        "🚀 rawtitle\ndescription: firstsecondthird\nactivity: runningtests\ncwd: /tmp/firstsecondspoof"
    );
    assert!(
        tooltip
            .lines()
            .all(|line| { !crate::session_timeline::metadata_has_forbidden_formatting(line) })
    );
    assert!(compose_tab_menu(&input).iter().all(|entry| match entry {
        TabMenuEntry::Header(label)
        | TabMenuEntry::Event { words: label, .. }
        | TabMenuEntry::RewindTo { words: label, .. } =>
            !crate::session_timeline::metadata_has_forbidden_formatting(label),
        TabMenuEntry::Action { label, .. }
        | TabMenuEntry::SessionAction { label, .. }
        | TabMenuEntry::ConnectionAction { label, .. }
        | TabMenuEntry::Upgrade { label, .. } =>
            !crate::session_timeline::metadata_has_forbidden_formatting(label),
        TabMenuEntry::Separator => true,
    }));
}

#[test]
fn the_tooltip_names_why_a_tab_needs_a_look() {
    let stalled = SessionChromeInput {
        attention: Some("Claude Code 2.1.283 -> 2.1.284 upgrade stalled".to_string()),
        ..full_input()
    };
    let tip = compose_tooltip(&stalled).expect("composes");
    assert!(
        tip.ends_with(
            "\ncwd: ~/src/aterm\nNeeds attention: Claude Code 2.1.283 -> 2.1.284 upgrade stalled"
        ),
        "{tip}"
    );
    let alone = SessionChromeInput {
        label: "zsh".to_string(),
        attention: Some("wedged on CI".to_string()),
        ..SessionChromeInput::default()
    };
    assert_eq!(
        compose_tooltip(&alone).as_deref(),
        Some("zsh\nNeeds attention: wedged on CI"),
        "a reason alone earns a tooltip"
    );
}

#[test]
fn the_reason_is_capped_and_sanitized() {
    let input = SessionChromeInput {
        label: "zsh".to_string(),
        attention: Some(format!("wedged\u{202e} on\nCI {}", "x".repeat(600))),
        ..SessionChromeInput::default()
    };
    let tip = compose_tooltip(&input).expect("composes");
    let line = tip.lines().nth(1).expect("the reason line");
    assert!(
        line.starts_with("Needs attention: wedged onCI xxx"),
        "{line}"
    );
    assert!(!crate::session_timeline::metadata_has_forbidden_formatting(
        line
    ));
    assert_eq!(tip.lines().count(), 2, "one line, no injected newline");
    assert!(
        line.chars().count()
            <= NEEDS_ATTENTION_PREFIX.chars().count() + DESCRIPTION_DISPLAY_MAX + 1
    );
}

#[test]
fn empty_metadata_degrades_to_no_tooltip() {
    let input = SessionChromeInput {
        label: "zsh".to_string(),
        ..SessionChromeInput::default()
    };
    assert_eq!(compose_tooltip(&input), None);
    let dashed = SessionChromeInput {
        label: "zsh".to_string(),
        state: Some("-".to_string()),
        icon: Some(String::new()),
        cwd: Some(String::new()),
        ..SessionChromeInput::default()
    };
    assert_eq!(compose_tooltip(&dashed), None);
}

#[test]
fn description_truncation_never_splits_a_grapheme_cluster() {
    let family = "👨\u{200D}👩\u{200D}👧";
    let mut input = full_input();
    input.description = Some(format!("{}{family}", "x".repeat(159)));
    let tip = compose_tooltip(&input).unwrap();
    let line = tip
        .lines()
        .nth(1)
        .unwrap()
        .strip_prefix("description: ")
        .unwrap();
    assert!(
        line.ends_with(family),
        "cluster at the cap boundary stays whole: {line:?}"
    );
    assert!(!line.ends_with('…'), "exactly at the cap ⇒ no ellipsis");

    input.description = Some(format!("{}{family}", "x".repeat(160)));
    let tip = compose_tooltip(&input).unwrap();
    let line = tip
        .lines()
        .nth(1)
        .unwrap()
        .strip_prefix("description: ")
        .unwrap();
    assert_eq!(
        line,
        format!("{}…", "x".repeat(160)),
        "over-cap cluster is dropped whole, cut lands on a cluster boundary"
    );

    input.description = Some(format!("{}e\u{301}zz", "x".repeat(159)));
    let tip = compose_tooltip(&input).unwrap();
    let line = tip
        .lines()
        .nth(1)
        .unwrap()
        .strip_prefix("description: ")
        .unwrap();
    assert!(
        line.contains("e\u{301}"),
        "combining accent stays attached to its base: {line:?}"
    );
    assert!(line.ends_with('…'), "the trailing `zz` is truncated away");
}

#[test]
fn timeline_tail_is_capped() {
    let mut input = full_input();
    input.timeline = (0..9)
        .map(|i| TimelineNote {
            kind: "state-change",
            at_ms: NOW - i * 1000,
        })
        .collect();
    let tip = compose_tooltip(&input).unwrap();
    let events = tip
        .lines()
        .filter(|l| l.starts_with("state changed"))
        .count();
    assert_eq!(events, 0, "the tooltip is no log: {tip:?}");
    let menu = menu_shown_at(&compose_tab_menu(&input), NOW);
    let headers = menu
        .iter()
        .filter(|e| matches!(e, TabMenuEntry::Header(h) if h.starts_with("state changed")))
        .count();
    assert_eq!(headers, TIMELINE_TAIL);
}

#[test]
fn a_recording_sessions_timeline_rows_open_rewind_and_a_plain_ones_stay_disabled() {
    let mut input = full_input();
    let plain = menu_shown_at(&compose_tab_menu(&input), NOW);
    assert!(
        !plain
            .iter()
            .any(|e| matches!(e, TabMenuEntry::RewindTo { .. })),
        "negative control: no history, no Rewind rows: {plain:?}"
    );
    assert!(plain.contains(&TabMenuEntry::Header(
        "changed directory \u{b7} 2m ago".into()
    )));
    input.rewind = true;
    let rows = menu_shown_at(&compose_tab_menu(&input), NOW);
    let rewind: Vec<_> = rows
        .iter()
        .filter_map(|e| match e {
            TabMenuEntry::RewindTo { words, at_ms, .. } => Some((words.as_str(), *at_ms)),
            _ => None,
        })
        .collect();
    assert_eq!(
        rewind,
        [
            ("details changed \u{b7} just now", NOW - 3_000),
            ("changed directory \u{b7} 2m ago", NOW - 125_000),
            ("started \u{b7} 2h ago", NOW - 7_200_000),
        ]
    );
    assert!(
        rows.iter()
            .filter(|e| matches!(e, TabMenuEntry::RewindTo { .. }))
            .all(|e| !crate::tab_menu::is_selectable(e)),
        "unbound templates cannot dispatch"
    );
    let mut bound = TabChromeExt {
        menu: rows.clone(),
        ..TabChromeExt::default()
    };
    bound.bind_terminal_origin(TerminalMenuOrigin {
        view: crate::tab_model::ViewId::from_stored(1),
        session: 2,
    });
    assert_eq!(
        bound
            .menu
            .iter()
            .filter(|e| matches!(e, TabMenuEntry::RewindTo { .. }))
            .filter(|e| crate::tab_menu::is_selectable(e))
            .count(),
        3,
        "each bound recording row can dispatch"
    );
    assert!(
        tab_menu_chrome_line(0, &compose_tab_menu(&input), NOW)
            .contains("\"changed directory \u{b7} 2m ago (rewind)\""),
        "the mirror says which rows rewind"
    );
}

#[test]
fn timeline_tail_leaves_out_agent_moves_and_typing() {
    let newest_first = [
        ("agent-change", 90),
        ("human", 90),
        ("fetch", 90),
        ("topic", 90),
        ("inbox-seen", 90),
        ("post-landed", 90),
        ("render", 90),
        ("program-state", 90),
        ("agent-change", 90),
        ("inbox", 90),
        ("cwd-change", 90),
    ];
    let tail = timeline_tail(newest_first.into_iter());
    let kinds: Vec<&str> = tail.iter().map(|n| n.kind).collect();
    assert_eq!(kinds, ["inbox", "cwd-change"]);
    assert!(tail.iter().all(|n| n.at_ms == 90), "each keeps its stamp");
    let mut input = full_input();
    input.timeline = [
        "agent-change",
        "task-change",
        "human",
        "agent-change",
        "cwd-change",
    ]
    .into_iter()
    .map(|kind| TimelineNote { kind, at_ms: NOW })
    .collect();
    let menu = menu_shown_at(&compose_tab_menu(&input), NOW);
    let rows: Vec<&str> = menu
        .iter()
        .filter_map(|e| match e {
            TabMenuEntry::Header(h) if h.ends_with("just now") => Some(h.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        rows,
        [
            "task changed \u{b7} just now",
            "changed directory \u{b7} just now"
        ]
    );
}

#[test]
fn menu_matches_tooltip_headers_and_orders_actions() {
    let mut input = full_input();
    input.connections = vec![fact(
        "s-peer",
        "build",
        ConnDirection::Outbound,
        ConnFlow::Push,
        false,
    )];
    let menu = menu_shown_at(&compose_tab_menu(&input), NOW);
    let tip = compose_tooltip(&input).unwrap();
    let tip_lines: Vec<&str> = tip.lines().filter(|l| !l.is_empty()).collect();
    let menu_headers: Vec<&str> = menu
        .iter()
        .filter_map(|e| match e {
            TabMenuEntry::Header(h) if h != "AGENT" => Some(h.as_str()),
            _ => None,
        })
        .collect();
    let tip_identity: Vec<&str> = tip_lines
        .iter()
        .copied()
        .filter(|l| !l.starts_with('\u{21e5}'))
        .collect();
    assert_eq!(&menu_headers[..tip_identity.len()], &tip_identity[..]);
    assert_eq!(
        menu_headers.len(),
        tip_identity.len() + 3,
        "+ the timeline tail"
    );
    let actions: Vec<MenuAction> =
        menu.iter()
            .filter_map(|e| match e {
                TabMenuEntry::Action { action, .. }
                | TabMenuEntry::SessionAction { action, .. } => Some(*action),
                _ => None,
            })
            .collect();
    assert_eq!(
        actions,
        vec![
            MenuAction::TakeOver,
            MenuAction::HarnessLog,
            MenuAction::SetRole,
            MenuAction::RenameSession,
            MenuAction::CopySessionId,
            MenuAction::CopyCwd,
            MenuAction::CloseTab,
        ]
    );
    assert_eq!(
        menu.iter()
            .filter(|e| matches!(e, TabMenuEntry::Separator))
            .count(),
        3,
        "identity | timeline | agent | actions"
    );
}

#[test]
fn the_agent_section_offers_the_hold_and_the_team_rows() {
    let rows = |input: &SessionChromeInput| -> Vec<(String, ConnVerb)> {
        compose_tab_menu(input)
            .iter()
            .filter_map(|e| match e {
                TabMenuEntry::ConnectionAction { peer_sid, verb, .. } => {
                    Some((peer_sid.as_str().to_string(), *verb))
                }
                _ => None,
            })
            .collect()
    };
    let first_agent_action = |input: &SessionChromeInput| {
        compose_tab_menu(input).iter().find_map(|e| match e {
            TabMenuEntry::SessionAction { action, .. } => Some(*action),
            _ => None,
        })
    };
    let mut plain = full_input();
    plain.sid = Some(SessionId::new("s-me"));
    assert!(
        compose_tab_menu(&plain)
            .iter()
            .any(|e| matches!(e, TabMenuEntry::Header(h) if h == "AGENT"))
    );
    assert_eq!(first_agent_action(&plain), Some(MenuAction::TakeOver));
    assert!(rows(&plain).is_empty(), "no team, no team rows");

    let mut held = plain.clone();
    held.agent.person_hold = true;
    assert_eq!(first_agent_action(&held), Some(MenuAction::HandBack));

    let mut worker = plain.clone();
    worker.agent.manager = Some((SessionId::new("s-boss"), "lab-sweep".to_string()));
    worker.agent.stoppable = true;
    assert_eq!(
        rows(&worker),
        [
            ("s-boss".to_string(), ConnVerb::Show),
            ("s-me".to_string(), ConnVerb::StopWorker),
        ]
    );

    let mut manager = plain.clone();
    manager.agent.workers = vec![
        (SessionId::new("s-w1"), "bench".to_string()),
        (SessionId::new("s-w2"), "fmt".to_string()),
    ];
    let menu = compose_tab_menu(&manager);
    let header = menu
        .iter()
        .position(|e| matches!(e, TabMenuEntry::Header(h) if h.starts_with("Workers")))
        .expect("the workers header");
    assert!(matches!(
        &menu[header],
        TabMenuEntry::Header(h) if h.contains('2')
    ));
    assert_eq!(
        rows(&manager),
        [
            ("s-w1".to_string(), ConnVerb::Show),
            ("s-w2".to_string(), ConnVerb::Show),
        ]
    );
}

#[test]
fn the_tooltip_carries_connection_lines_and_no_forged_one() {
    let mut peers = full_input();
    peers.connections = vec![
        fact(
            "s-b",
            "claude (b)",
            ConnDirection::Peer,
            ConnFlow::Push,
            false,
        ),
        fact(
            "s-c",
            "build",
            ConnDirection::Outbound,
            ConnFlow::Push,
            true,
        ),
    ];
    let tip = compose_tooltip(&peers).unwrap();
    assert!(tip.contains("\u{21c6} peer \"claude (b)\""));
    assert!(tip.contains("\u{21e5} pushes into \"build\" \u{00b7} live"));
    let mut hostile = full_input();
    hostile.connections = vec![fact(
        "s-evil",
        "a\u{202e}b\nAGENT",
        ConnDirection::Outbound,
        ConnFlow::Pull,
        false,
    )];
    let tip = compose_tooltip(&hostile).unwrap();
    assert!(
        tip.lines().any(|l| l == "\u{21e5} pulls from \"abAGENT\""),
        "controls/bidi stripped, no forged extra line: {tip:?}"
    );
}

#[test]
fn bare_session_menu_greys_unavailable_copies() {
    let input = SessionChromeInput {
        label: "zsh".to_string(),
        ..SessionChromeInput::default()
    };
    let menu = compose_tab_menu(&input);
    assert_eq!(menu[0], TabMenuEntry::Header("zsh".to_string()));
    assert_eq!(menu[1], TabMenuEntry::Separator);
    assert_eq!(
        menu.iter()
            .filter(|e| matches!(e, TabMenuEntry::Separator))
            .count(),
        2
    );
    let greyed = |a: MenuAction| {
        menu.iter().any(|e| {
            matches!(e, TabMenuEntry::SessionAction { action, enabled: false, .. } if *action == a)
        })
    };
    for a in [
        MenuAction::TakeOver,
        MenuAction::HarnessLog,
        MenuAction::SetRole,
        MenuAction::RenameSession,
        MenuAction::CopySessionId,
        MenuAction::CopyCwd,
    ] {
        assert!(greyed(a), "{a:?} greys without a session");
    }
    assert!(menu.iter().any(|e| matches!(
        e,
        TabMenuEntry::Action {
            action: MenuAction::CloseTab,
            enabled: true,
            ..
        }
    )));
}

#[test]
fn chrome_line_mirrors_composed_model() {
    let mut input = SessionChromeInput {
        label: "zsh".to_string(),
        cwd: Some("/tmp".to_string()),
        has_session: true,
        can_rename: true,
        sid: Some(SessionId::new("s-me")),
        ..SessionChromeInput::default()
    };
    input.agent.manager = Some((SessionId::new("s-boss"), "boss".to_string()));
    let menu = compose_tab_menu(&input);
    let line = tab_menu_chrome_line(2, &menu, NOW);
    let items: Vec<String> = menu_shown_at(&menu, NOW)
        .iter()
        .map(menu_entry_mirror)
        .collect();
    assert_eq!(line, format!("tab-menu tab=2 items={items:?}"));
    assert_eq!(items.iter().filter(|t| *t == "---").count(), 2);
    assert!(items.iter().any(|t| t.ends_with(" @s-boss")), "{items:?}");
    input.has_session = false;
    let greyed: Vec<String> = compose_tab_menu(&input)
        .iter()
        .map(menu_entry_mirror)
        .collect();
    assert!(
        greyed.iter().any(|t| t.ends_with(" (disabled)")),
        "{greyed:?}"
    );
}

#[test]
fn a_pending_upgrade_offers_the_owners_words_for_its_build() {
    use aterm_messages::UpgradeWord::{NotToday, Now, Skip};
    let mut input = SessionChromeInput {
        label: "claude".to_string(),
        has_session: true,
        ..SessionChromeInput::default()
    };
    let plain = compose_tab_menu(&input);
    input.upgrade = Some(UpgradeMenu {
        sid: "s-b5cf2faabac5ce5127bd".to_string(),
        moving: "Claude Code 2.1.281 → 2.1.282".to_string(),
        to: "2.1.282".to_string(),
        words: vec![Now, NotToday, Skip],
    });
    let menu = compose_tab_menu(&input);
    let start = menu
        .iter()
        .position(|e| *e == TabMenuEntry::Header("AGENT UPGRADE".to_string()))
        .expect("the section");
    assert_eq!(menu[start - 1], TabMenuEntry::Separator);
    assert_eq!(
        menu[start + 1],
        TabMenuEntry::Header("Claude Code 2.1.281 → 2.1.282".to_string())
    );
    let items: Vec<String> = menu[start + 2..start + 5]
        .iter()
        .map(menu_entry_mirror)
        .collect();
    assert_eq!(
        items,
        [
            "Upgrade Now (2.1.282)",
            "Not Today (2.1.282)",
            "Skip This Version (2.1.282)"
        ]
    );
    assert!(matches!(
        &menu[start + 4],
        TabMenuEntry::Upgrade { sid, to, word: Skip, .. }
            if to == "2.1.282" && sid == "s-b5cf2faabac5ce5127bd"
    ));
    assert_eq!(menu.len(), plain.len() + 6, "the section and nothing else");
    input.upgrade = Some(UpgradeMenu {
        words: Vec::new(),
        ..input.upgrade.clone().expect("set")
    });
    assert_eq!(compose_tab_menu(&input), plain, "no word: no section");
}

fn clause(head: &str, since_ms: u64, until_ms: Option<u64>) -> ClauseLine {
    ClauseLine {
        head: head.to_string(),
        since_ms,
        until_ms,
        by: None,
    }
}

#[test]
fn a_shown_label_names_its_author_after_its_words() {
    let mut input = full_input();
    let mut label = clause("restarting", NOW - 240_000, Some(NOW + 60_000));
    label.by = Some(ClauseAuthor::Harness);
    input.clause = Some(label.clone());
    assert!(
        compose_tooltip(&input)
            .expect("composes")
            .contains("\nnow: restarting \u{b7} by the harness\ncwd:"),
        "{:?}",
        compose_tooltip(&input)
    );
    assert_eq!(
        menu_shown_at(&compose_tab_menu(&input), NOW)[3],
        TabMenuEntry::Header("now: restarting \u{b7} by the harness \u{b7} 4m ago".to_string())
    );
    label.by = Some(ClauseAuthor::You);
    input.clause = Some(label.clone());
    assert!(
        compose_tooltip(&input)
            .expect("composes")
            .contains("\nnow: restarting \u{b7} by you\n")
    );
    let head = "x".repeat(48);
    label.head.clone_from(&head);
    input.clause = Some(label.clone());
    assert!(
        compose_tooltip(&input)
            .expect("composes")
            .contains(&format!("\nnow: {head} \u{b7} by you\n"))
    );
    label.by = None;
    input.clause = Some(label);
    let bare = compose_tooltip(&input).expect("composes");
    assert!(bare.contains(&format!("\nnow: {head}\ncwd:")), "{bare:?}");
    assert!(!bare.contains("\u{b7} by "), "{bare:?}");
}

#[test]
fn the_clause_is_a_now_line_after_activity_and_an_aged_menu_row() {
    let plain = full_input();
    let mut input = full_input();
    input.clause = Some(clause("running shell", NOW - 240_000, None));
    assert_eq!(
        compose_tooltip(&input).expect("composes"),
        "🤖 build agent\n\
         description: Rebuilds the docs site\n\
         activity: Building documentation\n\
         now: running shell\n\
         cwd: ~/src/aterm"
    );
    let menu = compose_tab_menu(&input);
    let without = compose_tab_menu(&plain);
    assert_eq!(menu.len(), without.len() + 1, "one row and nothing else");
    assert_eq!(
        menu[3],
        TabMenuEntry::Event {
            words: "now: running shell".to_string(),
            at_ms: NOW - 240_000,
        }
    );
    let mut rest = menu.clone();
    rest.remove(3);
    assert_eq!(rest, without, "every other row is today's");
    assert_eq!(
        menu_shown_at(&menu, NOW)[3],
        TabMenuEntry::Header("now: running shell \u{b7} 4m ago".to_string()),
        "the row is aged where it is shown"
    );
    input.activity = None;
    assert!(
        compose_tooltip(&input)
            .expect("composes")
            .contains("description: Rebuilds the docs site\nnow: running shell\ncwd:")
    );
    assert!(!compose_tooltip(&plain).expect("composes").contains("now:"));
    assert!(
        !without
            .iter()
            .any(|e| menu_entry_mirror(e).starts_with("now:"))
    );
}

#[test]
fn the_clauses_words_are_sanitized_like_every_header() {
    let mut input = full_input();
    input.clause = Some(clause("ship\u{202e}ping\u{7}", NOW, None));
    let tip = compose_tooltip(&input).expect("composes");
    assert!(tip.contains("\nnow: shipping\n"), "{tip:?}");
    assert!(!tip.contains('\u{202e}') && !tip.contains('\u{7}'));
    input.clause = Some(clause("\u{202e}\u{7}", NOW, None));
    assert_eq!(compose_tooltip(&input), compose_tooltip(&full_input()));
    assert_eq!(compose_tab_menu(&input), compose_tab_menu(&full_input()));
}

#[test]
fn a_shown_labels_until_bounds_the_compositions_expiry() {
    let mut input = full_input();
    assert_eq!(clock_expiry_ms(NOW, &input), None, "off and quiet: none");
    input.clause = Some(clause("running shell", NOW - 1_000, None));
    assert_eq!(clock_expiry_ms(NOW, &input), None, "a step arms nothing");
    input.clause = Some(clause("compiling", NOW - 1_000, Some(NOW + 9_000)));
    assert_eq!(clock_expiry_ms(NOW, &input), Some(NOW + 9_000));
    input.upgrade = Some(UpgradeMenu {
        sid: "s-1".to_string(),
        moving: "Claude Code 2.1.281 → 2.1.282".to_string(),
        to: "2.1.282".to_string(),
        words: Vec::new(),
    });
    assert_eq!(
        clock_expiry_ms(NOW, &input),
        Some(NOW + 9_000),
        "the earlier of the two"
    );
    input.clause = Some(clause("compiling", NOW, Some(NOW + 2 * CACHE_MAX_AGE_MS)));
    assert_eq!(clock_expiry_ms(NOW, &input), Some(NOW + CACHE_MAX_AGE_MS));
    input.clause = None;
    assert_eq!(
        clock_expiry_ms(NOW, &input),
        Some(NOW + CACHE_MAX_AGE_MS),
        "off: exactly the upgrade's term"
    );
}
