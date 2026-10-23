// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`aterm_agent::harness::upgrade_drive::Row::attention`] classification across all 41 upgrade states and their [`crate::message_reporters::upgrade_exceptions`] digest entries.
//! Classes are `Q` (quiet record), `F:kind` (fault), `D:kind` (digest entry), and `I:kind` (standing interrupt).

use aterm_agent::harness::upgrade::{Agent, GAVE_UP, Phase, Request};
use aterm_agent::harness::upgrade_drive::{Attention, HeldBy, Row};
use aterm_messages::Intent;
use aterm_messages::exceptions::Entry;

use crate::message_reporters::{UpgradeSpot, upgrade_exceptions};

const NOW: u64 = 1_790_640_000;
const H: u64 = 3_600;
/// Inspection ages for each state: 20 minutes, 2 hours, and 25 hours.
const AGES: [u64; 3] = [20 * 60, 2 * H, 25 * H];

fn holders(n: usize) -> Vec<HeldBy> {
    (0..n)
        .map(|i| HeldBy {
            pid: 4000 + i as u32,
            name: "zsh".into(),
            since: NOW - 600,
        })
        .collect()
}

fn state(agent: Agent, age: u64, phase: Phase, wait: &str) -> Row {
    Row {
        session: "07422bc3-1be9-4875-9672-2eaf7b28aa21".into(),
        tab: "s-3e79b5bde172ad0088ad".into(),
        from: "2.1.283".into(),
        to: "2.1.284".into(),
        source: "managed".into(),
        behind_since: NOW - age,
        minted_at: NOW - age,
        wait_since: if wait.is_empty() { 0 } else { NOW - age },
        wait: wait.into(),
        phase,
        agent,
        noticed_at: if age > 3 * H { NOW - 60 } else { 0 },
        ..Row::default()
    }
}

/// State `id`, `age` seconds after the move began to wait.
fn fixture(id: &str, age: u64) -> Row {
    let wait_row = |agent: Agent, announced: bool, wait: &str| {
        let phase = if announced {
            Phase::Announced {
                at_s: NOW - 600,
                asks: 1,
            }
        } else {
            Phase::Pending
        };
        let mut r = state(agent, age, phase, wait);
        if matches!(wait, "background" | "not-idle:shell") {
            r.held_by = holders(16);
        }
        if wait.starts_with("announce-refused") {
            r.refused = 5;
            r.refused_at = NOW - age;
        }
        r
    };
    let failed = |why: &str| state(Agent::Claude, age, Phase::Failed(why.into()), "");
    let claude = |wait: &str| wait_row(Agent::Claude, false, wait);
    let codex = |wait: &str| wait_row(Agent::Codex, false, wait);
    match id {
        "G01" => claude("settling"),
        "G02" => claude("attended"),
        "G03" => claude("not-idle:busy"),
        "G04" => claude("not-idle:shell"),
        "G05" => claude("not-idle:waiting"),
        "G06" => claude("status-stale:busy"),
        "G07" => claude("held"),
        "G08" => claude("limited"),
        "G09" => claude("login"),
        "G10" => claude("queued"),
        "G11" => claude("draft"),
        "G12" => claude("box"),
        "G13" => claude("background"),
        "G14" => wait_row(Agent::Claude, true, "awaiting-ready"),
        "G15" => wait_row(Agent::Claude, true, "release:awaiting-ready"),
        "G16" => claude("announce-refused:changed"),
        "G17" => claude("no-socket"),
        "G18" => claude("not-foreground"),
        "G19" => claude("conversation-owner-ambiguous"),
        "G20" => claude("changed-before-signal"),
        "G21" => claude("model-cache-warm"),
        "H01" => claude("terminal:tmux"),
        "H02" => failed("not-a-shell-job"),
        "H03" => failed("argv:--foo"),
        "H04" => failed("line:too-long"),
        "S01" => failed("unanswered"),
        "S02" => {
            let mut r = failed("unanswered");
            r.late_ready = true;
            r.wait = "background".into();
            r.wait_since = NOW - age;
            r.held_by = holders(3);
            r
        }
        "S03" => {
            let mut r = failed("unanswered");
            r.release = "awaiting-ready".into();
            r
        }
        "S04" => failed("signal-refused"),
        "S05" => {
            let mut r = failed("signal-refused");
            r.streak_why = "signal-refused".into();
            r.stop_streak = 2;
            r
        }
        "S06" => {
            let mut r = failed("stale-exit");
            r.exited_at = NOW - 2 * H;
            r
        }
        "F01" => state(Agent::Claude, age, Phase::Exiting { at_s: NOW - 120 }, ""),
        "F02" => state(
            Agent::Claude,
            age,
            Phase::Exiting {
                at_s: NOW - 20 * 60,
            },
            "",
        ),
        "F03" => {
            let mut r = state(Agent::Claude, age, Phase::Exiting { at_s: NOW - 360 }, "");
            r.exited_at = NOW - 300;
            r
        }
        "F04" => {
            let mut r = state(
                Agent::Claude,
                age,
                Phase::Relaunched {
                    at_s: NOW - 20 * 60,
                },
                "",
            );
            r.exited_at = NOW - 20 * 60 - 5;
            r
        }
        "F05" => state(Agent::Claude, age, Phase::Done, ""),
        "F06" => {
            let mut r = state(Agent::Claude, age, Phase::Done, "");
            r.outcome = "done:no-continue".into();
            r
        }
        "O01" => {
            let mut r = claude("background");
            r.request = Request::Skip("2.1.284".into());
            r
        }
        "O02" => {
            let mut r = claude("background");
            r.request = Request::DeferUntil(NOW + H);
            r
        }
        "X11" => codex("thread-locked"),
        other => panic!("no such state {other}"),
    }
}

/// The class a row has, in the table's spelling.
fn class(row: &Row) -> String {
    match row.attention(NOW) {
        Attention::Quiet => "Q".into(),
        Attention::Fault(n) => format!("F:{}", n.kind),
        Attention::Digest(n) => format!("D:{}", n.kind),
        Attention::Interrupt(n) => format!("I:{}", n.kind),
    }
}

/// Verifies the attention classification of all 41 upgrade states across [`AGES`].
#[test]
fn every_upgrade_state_has_its_attention() {
    #[rustfmt::skip]
    let table: [(&str, [&str; 3]); 40] = [
        ("G01", ["Q", "F:harness-stuck", "F:harness-stuck"]),
        ("G02", ["Q", "Q", "Q"]),
        ("G03", ["Q", "Q", "D:own-work"]),
        ("G04", ["Q", "Q", "D:own-work"]),
        ("G05", ["Q", "Q", "Q"]),
        ("G06", ["Q", "Q", "D:own-work"]),
        ("G07", ["Q", "Q", "Q"]),
        ("G08", ["Q", "Q", "Q"]),
        ("G09", ["Q", "Q", "Q"]),
        ("G10", ["Q", "Q", "Q"]),
        ("G11", ["Q", "Q", "D:typed-draft"]),
        ("G12", ["Q", "Q", "Q"]),
        ("G13", ["Q", "Q", "D:own-work"]),
        ("G14", ["Q", "F:harness-stuck", "F:harness-stuck"]),
        ("G15", ["Q", "F:harness-stuck", "F:harness-stuck"]),
        ("G16", ["Q", "F:harness-stuck", "F:harness-stuck"]),
        ("G17", ["Q", "F:harness-stuck", "F:harness-stuck"]),
        ("G18", ["Q", "D:suspended", "D:suspended"]),
        ("G19", ["Q", "F:harness-stuck", "F:harness-stuck"]),
        ("G20", ["Q", "F:harness-stuck", "F:harness-stuck"]),
        ("G21", ["Q", "F:harness-stuck", "F:harness-stuck"]),
        ("H01", ["D:no-terminal", "D:no-terminal", "D:no-terminal"]),
        ("H02", ["D:refused-launch", "D:refused-launch", "D:refused-launch"]),
        ("H03", ["D:refused-launch", "D:refused-launch", "D:refused-launch"]),
        ("H04", ["D:refused-launch", "D:refused-launch", "D:refused-launch"]),
        ("S01", ["Q", "Q", "Q"]),
        ("S02", ["Q", "Q", "Q"]),
        ("S03", ["Q", "Q", "Q"]),
        ("S04", ["Q", "Q", "Q"]),
        ("S05", ["D:repeating-stop", "D:repeating-stop", "D:repeating-stop"]),
        ("S06", ["I:agent-down", "I:agent-down", "I:agent-down"]),
        ("F01", ["Q", "Q", "Q"]),
        ("F02", ["I:hung-exit", "I:hung-exit", "I:hung-exit"]),
        ("F03", ["Q", "Q", "Q"]),
        ("F04", ["F:relaunch-stuck", "F:relaunch-stuck", "F:relaunch-stuck"]),
        ("F05", ["Q", "Q", "Q"]),
        ("F06", ["Q", "Q", "Q"]),
        ("O01", ["Q", "Q", "Q"]),
        ("O02", ["Q", "Q", "Q"]),
        ("X11", ["Q", "Q", "Q"]),
    ];
    let mut wrong = Vec::new();
    for (id, want) in table {
        for (age, want) in AGES.iter().zip(want) {
            let got = class(&fixture(id, *age));
            if got != want {
                wrong.push(format!("{id} at {} h: {got}, want {want}", age / H));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    let cannot = aterm_agent::harness::upgrade_drive::Need::upgrade_cannot_run("state-unwritable");
    assert_eq!(cannot.kind, "upgrade-cannot-run");
}

/// Relaunched and exited agents escalate from quiet or fault states to interrupts at their expiry bounds.
#[test]
fn the_relaunch_and_the_returned_prompt_reach_their_interrupts_on_time() {
    let relaunched = |waited: u64| {
        let mut r = state(
            Agent::Claude,
            7 * H,
            Phase::Relaunched { at_s: NOW - waited },
            "",
        );
        r.exited_at = NOW - waited - 5;
        r
    };
    assert_eq!(class(&relaunched(6 * 60)), "Q");
    assert_eq!(class(&relaunched(20 * 60)), "F:relaunch-stuck");
    assert_eq!(class(&relaunched(40 * 60)), "I:relaunch-stuck");
    let exited = |waited: u64| {
        let mut r = state(
            Agent::Claude,
            7 * H,
            Phase::Exiting { at_s: NOW - waited },
            "",
        );
        r.exited_at = NOW - waited;
        r
    };
    assert_eq!(class(&exited(4 * 60)), "Q", "a person may be typing there");
    assert_eq!(class(&exited(6 * 60)), "I:agent-down");
    for why in ["no-rollout", "codex-lane-retired"] {
        let mut r = state(Agent::Codex, 7 * H, Phase::Failed(why.into()), "");
        r.exited_at = NOW - 2 * H;
        assert_eq!(class(&r), "I:agent-down", "{why}");
    }
}

/// Across all states and ages, only `agent-down`, `hung-exit`, and `relaunch-stuck` produce an interrupt.
#[test]
fn no_state_but_an_ended_agent_ever_interrupts() {
    let ids = [
        "G01", "G02", "G03", "G04", "G05", "G06", "G07", "G08", "G09", "G10", "G11", "G12", "G13",
        "G14", "G15", "G16", "G17", "G18", "G19", "G20", "G21", "H01", "H02", "H03", "H04", "S01",
        "S02", "S03", "S04", "S05", "S06", "F01", "F02", "F03", "F04", "F05", "F06", "O01", "O02",
        "X11",
    ];
    let mut interrupts = std::collections::BTreeSet::new();
    for id in ids {
        for age in [60, 20 * 60, 2 * H, 25 * H, 9 * 86_400, 400 * 86_400] {
            if let Attention::Interrupt(need) = fixture(id, age).attention(NOW) {
                assert!(
                    ["agent-down", "hung-exit", "relaunch-stuck"].contains(&need.kind),
                    "{id} at {age} s is an interrupt of kind {}",
                    need.kind
                );
                interrupts.insert(id);
            }
        }
    }
    assert_eq!(
        interrupts.into_iter().collect::<Vec<_>>(),
        ["F02", "S06"],
        "the ended agent and the shutdown that hung (F04 and F03 are the returned agent's looks)"
    );
}

/// Unknown wait strings classify only as quiet records or faults, never digest entries or interrupts.
#[test]
fn an_unknown_wait_is_a_record() {
    for agent in [Agent::Claude, Agent::Codex] {
        for announced in [false, true] {
            for age in [60, 20 * 60, 2 * H, 25 * H, 9 * 86_400, 400 * 86_400] {
                for wait in ["zzz-unclassified", "daemon-first:zzz"] {
                    let phase = if announced {
                        Phase::Announced {
                            at_s: NOW - 600,
                            asks: 1,
                        }
                    } else {
                        Phase::Pending
                    };
                    let row = state(agent, age, phase, wait);
                    let got = class(&row);
                    assert!(
                        got == "Q" || got.starts_with("F:"),
                        "{wait} at {age} s: {got}"
                    );
                    assert!(
                        !got.starts_with("I:"),
                        "an unknown wait interrupts: {wait} {got}"
                    );
                }
            }
        }
    }
}

/// The spot of a tab, for a row: `in tab N` and the navigation to it.
fn spot(n: u16) -> UpgradeSpot {
    UpgradeSpot {
        place: format!("in tab {n}"),
        nav: Some(Intent::ShowTab { tab: n, window: 1 }),
    }
}

/// The entries the digest gets for `rows`, each in `tab N`.
fn entries(rows: &[Row]) -> Vec<Entry> {
    let placed: Vec<(&Row, UpgradeSpot)> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| (row, spot(u16::try_from(i + 1).expect("few tabs"))))
        .collect();
    upgrade_exceptions(&placed, NOW)
}

/// A row of state `id` at a day, as tab `tab`.
fn day_row(id: &str, tab: &str) -> Row {
    Row {
        tab: tab.into(),
        ..fixture(id, 25 * H)
    }
}

/// The digest states of the table, at every age each can be one.
const DIGEST_IDS: [&str; 11] = [
    "G03", "G04", "G06", "G11", "G13", "G18", "H01", "H02", "H03", "H04", "S05",
];

/// The places a tab can be named: the short and the long forms.
const PLACES: [&str; 4] = [
    "in tab 1",
    "in tab 12",
    "in window 2, tab 1",
    "in window 12, tab 10",
];

/// Builds every `(kind, Entry)` pair across [`DIGEST_IDS`], ages, and [`PLACES`].
pub(crate) fn every_digest_entry() -> Vec<(&'static str, Entry)> {
    let mut all = Vec::new();
    for id in DIGEST_IDS {
        for age in [20 * 60, 2 * H, 25 * H, 9 * 86_400] {
            let row = fixture(id, age);
            let Attention::Digest(need) = row.attention(NOW) else {
                continue;
            };
            for place in PLACES {
                let spot = UpgradeSpot {
                    place: place.to_string(),
                    nav: Some(Intent::ShowTab { tab: 1, window: 1 }),
                };
                for entry in upgrade_exceptions(&[(&row, spot)], NOW) {
                    all.push((need.kind, entry));
                }
            }
        }
    }
    all
}

/// Every upgrade digest entry builds a valid [`Entry::message`] under the `harness.upgrade.` key prefix.
#[test]
fn every_digest_state_says_why_and_what() {
    let all = every_digest_entry();
    for (kind, entry) in &all {
        let msg = entry
            .message()
            .unwrap_or_else(|fault| panic!("{kind} {}: {fault:?}", entry.key()));
        assert!(
            entry.key().starts_with("harness.upgrade."),
            "{}",
            entry.key()
        );
        assert!(!msg.detail.is_empty(), "the reason and the action");
    }
    assert!(
        all.len() > 100,
        "the roster covers the states: {}",
        all.len()
    );
    let cannot = aterm_agent::harness::upgrade_drive::Need::upgrade_cannot_run("state-unwritable");
    let entry = Entry::new(
        aterm_messages::exceptions::Family::UpgradeHost,
        "",
        "cannot-run",
        cannot.title,
        aterm_messages::need::Why::OnlyYou(aterm_messages::need::Reason::new(&cannot.why)),
        aterm_messages::need::Then::step(&cannot.then),
    );
    assert!(entry.message().is_ok(), "{:?}", entry.message());
}

/// Multi-tab kinds collapse into one entry per kind regardless of input order, while per-tab actions emit one entry per tab.
#[test]
fn several_tabs_are_one_line_per_kind_and_one_line_per_tab_where_the_action_is_per_tab() {
    let mut rows = vec![
        day_row("G04", "s-a"),
        day_row("G13", "s-b"),
        day_row("G03", "s-c"),
        day_row("G11", "s-d"),
        day_row("G18", "s-e"),
        day_row("G18", "s-f"),
        day_row("H01", "s-g"),
    ];
    for (i, row) in rows.iter_mut().enumerate() {
        row.behind_since = NOW - (30 + i as u64) * H;
    }
    let got = entries(&rows);
    let keys: Vec<String> = got.iter().map(Entry::key).collect();
    assert_eq!(
        keys,
        [
            "harness.upgrade.s-g.no-terminal",
            "harness.upgrade.own-work",
            "harness.upgrade.s-e.suspended",
            "harness.upgrade.s-f.suspended",
            "harness.upgrade.typed-draft",
        ],
        "own-work is ONE entry for three tabs; typed-draft one; the rest one per tab"
    );
    let own = got[1].message().expect("passes");
    assert!(own.title.contains("3 tabs"), "{}", own.title);
    assert!(
        own.detail.iter().any(|l| l.contains("tab 3, tab 2, tab 1")),
        "the tabs are named behind Details, most behind first: {:?}",
        own.detail
    );
    assert!(
        own.actions.is_empty(),
        "several tabs: no single tab to show"
    );
    rows.reverse();
    let placed: Vec<(&Row, UpgradeSpot)> = rows.iter().map(|row| (row, spot(1))).collect();
    let again: Vec<String> = upgrade_exceptions(&placed, NOW)
        .iter()
        .map(Entry::key)
        .collect();
    assert_eq!(again, keys);
    let one = entries(&[day_row("G13", "s-b")]);
    let msg = one[0].message().expect("passes");
    assert!(msg.title.contains("tab 1"), "{}", msg.title);
    assert_eq!(msg.actions, [Intent::ShowTab { tab: 1, window: 1 }]);
    let far = upgrade_exceptions(
        &[(
            &day_row("G13", "s-b"),
            UpgradeSpot {
                place: "in window 12, tab 10".into(),
                nav: None,
            },
        )],
        NOW,
    );
    let title = far[0].message().expect("passes").title;
    assert!(
        aterm_messages::text::glass_title_fault(&title).is_none(),
        "{title}"
    );
    assert!(title.contains("window 12"), "{title}");
}

/// Only digest states emit entries, preserving each kind's severity and resurface span.
#[test]
fn only_a_digest_state_is_an_entry_and_its_severity_and_span_follow_its_kind() {
    for id in [
        "G01", "G02", "G05", "G08", "S01", "S06", "F02", "F04", "F05", "O01",
    ] {
        for age in AGES {
            let row = fixture(id, age);
            let got = upgrade_exceptions(&[(&row, spot(1))], NOW);
            assert!(
                got.is_empty(),
                "{id} at {} h is {}: {:?}",
                age / H,
                class(&row),
                got.iter().map(Entry::key).collect::<Vec<_>>()
            );
        }
    }
    let severity_of = |id: &str| {
        entries(&[fixture(id, 25 * H)])[0]
            .message()
            .expect("passes")
            .severity
    };
    assert_eq!(severity_of("S05"), aterm_messages::Severity::Warn);
    assert_eq!(severity_of("G13"), aterm_messages::Severity::Info);
    let span = |id: &str| match entries(&[fixture(id, 25 * H)])[0]
        .message()
        .expect("passes")
        .need()
    {
        aterm_messages::need::Need::Later(ask) => ask.resurface(),
        other => panic!("{id}: {other:?}"),
    };
    use aterm_messages::need::Resurface;
    for choice in ["H01", "H02"] {
        assert_eq!(span(choice), Resurface::CHOICE, "{choice}");
    }
    for wait in ["G13", "G11", "G18", "S05"] {
        assert_eq!(span(wait), Resurface::WAIT, "{wait}");
    }
    let mut gave_up = fixture("G13", 30 * H);
    gave_up.phase = Phase::Failed(GAVE_UP.into());
    gave_up.streak_why = GAVE_UP.into();
    gave_up.stop_streak = 2;
    assert_eq!(class(&gave_up), "D:own-work");
}
