// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Node-log producer and schema-key leak tests.

use std::collections::BTreeMap;

use aterm_spec::wait::{MUST_HAPPEN, until_within};
use aterm_update::recovery_ledger::EndClass;

use super::*;

fn host() -> &'static NodeLogHost {
    let clock = Clock::reading(|| aterm_slog::Stamp::new(1_760_000_000_000, 5));
    Box::leak(Box::new(NodeLogHost::started(1_760_000_000, clock)))
}

const PLANTED: &str = "zebra-quartz-0517";

fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn the_start_is_first_and_the_ledger_amends_it() {
    let h = host();
    h.note_prev_end(EndClass::Signal(11));
    h.note_prev_end(EndClass::None);
    let lines = h.lines();
    assert!(
        lines[0].starts_with("1 open n start prev_end=- by=aterm src=aterm conf=exact t="),
        "{lines:?}"
    );
    assert!(lines[1].starts_with("2 amend n start prev_end=signal code=11 by=aterm"));
    assert!(lines[2].starts_with("3 amend n start prev_end=none by=aterm"));
}

#[test]
fn every_fabric_link_transition_reaches_the_node_log() {
    install();
    crate::fabric::with_link_reset(|| {
        let before = node_log().last_seq();
        let store = crate::session_store::new_store();
        let generation = crate::fabric::next_bridge_generation();
        crate::fabric::bridge_attached(generation);
        assert!(crate::fabric::link_report(
            &store,
            Some(generation),
            true,
            "",
            Some(2)
        ));
        crate::fabric::bridge_lost(&store, generation);
        let words = until_within(MUST_HAPPEN, "the link's moves in the node log", || {
            let moves: Vec<String> = node_log()
                .lines()
                .into_iter()
                .filter(|l| {
                    l.split(' ').next().and_then(|s| s.parse::<u64>().ok()) > Some(before)
                        && l.contains(" mark n link ")
                })
                .map(|l| {
                    let state = l.split(" state=").nth(1).and_then(|s| s.split(' ').next());
                    let by = l.split(" by=").nth(1).and_then(|s| s.split(' ').next());
                    format!("{}:{}", state.unwrap_or("?"), by.unwrap_or("?"))
                })
                .collect();
            (moves.len() >= 3).then_some(moves)
        });
        assert_eq!(
            words,
            ["stalled:aterm", "connected:bridge", "disconnected:aterm"],
            "attach, the bridge's report, the loss"
        );
    });
}

#[test]
fn a_config_snapshot_names_the_keys_it_moved_and_never_a_value() {
    let mut app = crate::App::headless_for_test();
    let h = host();
    app.node_log = h;
    let text = format!(
        "[harness]\nanswer_text = \"{PLANTED}\"\n\n[palette_overrides]\nzebra-person = \"x\"\n\n\
         [zebra_unknown_table]\nquartz = 1\n"
    );
    let snapshot = crate::native_config_service::VersionedConfigService::new(text)
        .expect("valid TOML")
        .snapshot();
    app.publish_native_config_snapshot(&snapshot);
    let lines = until_within(MUST_HAPPEN, "the config keys in the node log", || {
        let lines = h.lines();
        (lines.len() >= 4).then_some(lines)
    });
    let marks: Vec<String> = lines
        .iter()
        .skip(1)
        .map(|l| l.split(" by=").next().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        marks,
        [
            "2 mark n config key=harness.answer_text",
            "3 mark n config key=palette_overrides",
            "4 mark n config n=1",
        ]
    );
    for l in &lines {
        for secret in [PLANTED, "zebra", "quartz"] {
            assert!(!l.contains(secret), "{secret} leaked: {l}");
        }
    }
    app.publish_native_config_snapshot(&snapshot);
    assert_eq!(h.lines().len(), 4, "nothing moved, nothing written");
}

#[test]
fn changed_config_keys_name_schema_rows_only() {
    let before = values(&[
        ("harness", "t1"),
        ("harness.answer_text", "a"),
        ("font_px", "12"),
    ]);
    let after = values(&[
        ("harness", "t2"),
        ("harness.answer_text", PLANTED),
        ("palette_overrides", "t"),
        ("palette_overrides.someone", "x"),
        ("no_such_key", "1"),
    ]);
    let (names, unnamed) = changed_config_keys(&before, &after);
    assert_eq!(
        names.into_iter().collect::<Vec<_>>(),
        ["font_px", "harness.answer_text", "palette_overrides"],
        "font_px removed, the text changed, the map added; `harness` is a parent"
    );
    assert_eq!(unnamed, 1, "no_such_key");
    let (none, zero) = changed_config_keys(&after, &after);
    assert!(none.is_empty() && zero == 0);

    let h = host();
    let many: std::collections::BTreeSet<&'static str> =
        crate::native_config_language::config_schema()
            .iter()
            .map(|e| e.key)
            .take(CONFIG_KEYS_MAX + 3)
            .collect();
    h.note_config(&many, 2);
    let lines = h.lines();
    assert_eq!(lines.len(), 1 + CONFIG_KEYS_MAX + 1);
    assert!(
        lines[CONFIG_KEYS_MAX + 1].contains(" config n=5 "),
        "{lines:?}"
    );
}

#[test]
fn every_schema_key_fits_the_key_field() {
    for e in crate::native_config_language::config_schema() {
        assert!(
            aterm_slog::Fields::new()
                .id(aterm_slog::Key::CfgKey, e.key)
                .is_ok(),
            "{}",
            e.key
        );
    }
}

#[test]
fn no_node_log_record_is_bus_class() {
    let h = host();
    h.note_prev_end(EndClass::Clean);
    h.note_link(LinkMove {
        state: "connected",
        generation: 3,
        said: LinkSaid::Report,
    });
    h.note_config(&["font_px"].into_iter().collect(), 1);
    let recs = h.with(|i| i.log.window().cloned().collect::<Vec<_>>());
    assert_eq!(recs.len(), 5);
    assert!(recs.iter().all(|r| !r.is_bus_class() && !r.is_turn_class()));
}

#[test]
fn log_n_prints_the_node_log_as_log_prints_a_session() {
    let h = host();
    h.note_link(LinkMove {
        state: "connected",
        generation: 4,
        said: LinkSaid::Report,
    });
    h.note_config(&["font_px"].into_iter().collect(), 0);
    let all = cmd_node_log(h, "n");
    let mut lines = all.lines();
    assert_eq!(
        lines.next(),
        Some("OK 3 v=2 as_of=3 ep=1760000000 oldest=1"),
        "{all}"
    );
    let body: Vec<&str> = lines
        .map(|l| l.split(" by=").next().unwrap_or_default())
        .collect();
    assert_eq!(
        body,
        [
            "1 open n start prev_end=-",
            "2 mark n link state=connected gen=4",
            "3 mark n config key=font_px",
        ]
    );
    let on = cmd_node_log(h, "since=2 ep=1760000000 n");
    assert!(on.starts_with("OK 1 v=2 as_of=3 "), "{on}");
    let newest = cmd_node_log(h, "n n=1");
    assert!(
        newest.starts_with("OK 1 v=2 as_of=3 ep=1760000000 oldest=1 more=2\n"),
        "{newest}"
    );
    let links = cmd_node_log(h, "kinds=link n");
    assert_eq!(links.lines().count(), 2, "{links}");
    let json = cmd_node_log(h, "n --json");
    assert!(
        json.lines()
            .skip(1)
            .all(|l| l.starts_with('{') && l.contains("\"node\":\"n\"")),
        "{json}"
    );
    for bad in ["n lvl=turn", "n class=turn", "n t3", "n since=2"] {
        assert!(cmd_node_log(h, bad).starts_with("ERR usage: log"), "{bad}");
    }
    let other = cmd_node_log(h, "n since=2 ep=7");
    assert!(
        other
            .lines()
            .nth(1)
            .is_some_and(|l| l.starts_with("gap why=reset lost=0 ")),
        "{other}"
    );

    let clock = || Clock::reading(|| aterm_slog::Stamp::new(1_760_000_000_000, 5));
    let previous = NodeLogHost::started(instance_epoch("p4242-000000000000000000000001"), clock());
    let current = NodeLogHost::started(instance_epoch("p4242-000000000000000000000002"), clock());
    for host in [&previous, &current] {
        host.note_link(LinkMove {
            state: "stalled",
            generation: 1,
            said: LinkSaid::Attached,
        });
    }
    let cursor = previous.with(|i| (i.log.last_seq(), i.log.ep()));
    let resumed = cmd_node_log(&current, &format!("n since={} ep={}", cursor.0, cursor.1));
    assert!(
        resumed.lines().any(|l| l.starts_with("gap why=reset ")),
        "the predecessor's cursor must reset: {resumed}"
    );
    assert!(
        resumed.lines().any(|l| l.contains(" open n start ")),
        "the new process's start cannot be silently skipped: {resumed}"
    );
    let ep = current.with(|i| i.log.ep());
    let continued = cmd_node_log(&current, &format!("n since=1 ep={ep}"));
    assert!(
        !continued.lines().any(|l| l.starts_with("gap ")),
        "{continued}"
    );
    assert!(
        continued
            .lines()
            .nth(1)
            .is_some_and(|l| l.contains(" mark n link "))
    );
}

#[test]
fn log_n_tells_a_reader_what_the_ring_evicted() {
    let h = host();
    for g in 0..aterm_slog::NODE_LOG_RECORDS as u64 + 1 {
        h.note_link(LinkMove {
            state: "stalled",
            generation: g,
            said: LinkSaid::Attached,
        });
    }
    let all = cmd_node_log(h, "n n=1");
    let mut lines = all.lines();
    assert_eq!(
        lines.next(),
        Some("OK 2 v=2 as_of=1026 ep=1760000000 oldest=3 lost=2 more=1023"),
        "{all}"
    );
    assert_eq!(lines.next(), Some("gap why=evicted lost=2 resync_to=2"));
}
