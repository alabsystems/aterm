// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unit tests for `crate::fact_menu` rows and the traffic card.

use std::path::PathBuf;

use aterm_agent::harness::footer::{FactKind, FactMask};
use aterm_agent::harness::outlook::Figure;
use aterm_agent::harness::session_usage::TrafficView;

use super::{
    AccountLabel, AccountMenuRow, FactAct, FactMenuFacts, PoolRead, SessionWeek,
    account_menu_rows_from, band_glyphs, band_level, rows, traffic_rows, weekly_facts,
    weekly_gap_why,
};
use crate::autopilot_menu::{MenuAct, MenuRow, PlotInk};
use crate::claude_lights::Fact;

/// An account row's name and standing on one line, as the tests below read it.
trait Standing {
    fn standing(&self) -> String;
}

impl Standing for AccountMenuRow {
    fn standing(&self) -> String {
        format!("{} \u{b7} {}", self.name, self.trail)
    }
}

/// The rows of one read of `cli`'s pool as the card's worker makes it ([`PoolRead::load`], then
/// its rows).
fn pool_menu_rows(
    cli: aterm_agent::harness::accounts::Cli,
    places: (&std::path::Path, &std::path::Path, Option<&std::path::Path>),
    session: (Option<&str>, Option<&str>),
    week: SessionWeek,
    now: i64,
) -> Result<Vec<AccountMenuRow>, String> {
    Ok(PoolRead::load(cli, places, now)?
        .rows(session, week, now)
        .rows)
}

#[test]
fn unavailable_manual_admission_disables_only_signin_choices_and_next() {
    use aterm_agent::harness::accounts::{self, ManualSigninCapability};
    let roster = accounts::Roster {
        rows: ["ready", "signed-out", "missing"]
            .map(|label| accounts::Row {
                label: label.into(),
                email: Some(format!("{label}@example.test")),
                ..Default::default()
            })
            .to_vec(),
    };
    let mut state = accounts::State {
        active: Some("ready".into()),
        pinned: true,
        ..Default::default()
    };
    state.lapsed_logins.insert("signed-out".into());
    let before = state.clone();
    let enrolled = std::collections::BTreeSet::from(["ready".into(), "signed-out".into()]);
    let initial = account_menu_rows_from(
        &roster,
        &state,
        &enrolled,
        true,
        None,
        None,
        SessionWeek::default(),
        100,
    );
    assert!(initial.iter().all(|row| row.enabled));
    let mut available = initial.clone();
    super::apply_manual_signin_capability(
        &mut available,
        &roster,
        &state,
        &enrolled,
        100,
        ManualSigninCapability::Available,
    );
    assert_eq!(available, initial);
    let mut unavailable = initial;
    super::apply_manual_signin_capability(
        &mut unavailable,
        &roster,
        &state,
        &enrolled,
        100,
        ManualSigninCapability::Unavailable,
    );
    assert_eq!(
        unavailable
            .iter()
            .map(|row| row.enabled)
            .collect::<Vec<_>>(),
        [true, false, false]
    );
    assert!(unavailable[0].active);
    let facts = FactMenuFacts {
        cli: Some(accounts::Cli::Claude),
        accounts: unavailable,
        manual_signin_unavailable: true,
        ..Default::default()
    };
    let menu = rows(Fact::Account, &facts);
    assert!(menu.iter().any(|row| matches!(
        row,
        MenuRow::Item {
            act: MenuAct::Fact(Fact::Account, FactAct::AutoRotate),
            enabled: false,
            ..
        }
    )));
    assert!(menu.iter().any(|row| matches!(row, MenuRow::Item { act: MenuAct::Fact(Fact::Account, FactAct::UseAccount(label)), enabled: true, .. } if label.as_str() == "ready")));
    assert_eq!(
        state, before,
        "capability projection never clears the preferred pin"
    );
    let empty = FactMenuFacts {
        cli: Some(accounts::Cli::Claude),
        manual_signin_unavailable: true,
        ..Default::default()
    };
    assert!(rows(Fact::Account, &empty).iter().any(|row| matches!(
        row,
        MenuRow::Item {
            act: MenuAct::Fact(Fact::Account, FactAct::AutoRotate),
            enabled: false,
            ..
        }
    )));
}

/// A sign-in's last failure is clipboard evidence, not a card row: Copy details holds its
/// words whole, the account it was for and when the attempt finished on both clocks; the card
/// itself draws none of it.
#[test]
fn a_typed_login_diagnostic_is_preserved_in_copy_and_is_no_card_row() {
    use aterm_agent::harness::{
        account_progress::AccountProgress,
        accounts::Row,
        accounts_sync::LoginDiagnostic,
        open_url::{Met, MetWord},
    };
    let failed = |label: &str, at: i64, word: MetWord| {
        LoginDiagnostic::from_exit_for_test(
            &Row {
                label: label.into(),
                ..Row::default()
            },
            at,
            1,
            Some(Met::new(word, None)),
        )
    };
    let older = failed("alpha", 70, MetWord::Stall);
    let newest = failed("zeta", 90, MetWord::BrowserControl);
    let mut progress = AccountProgress::unknown(100);
    progress.login_diagnostics = vec![older.clone(), newest.clone()];
    let facts = FactMenuFacts {
        account_progress: Some(progress),
        now: 100,
        ..menu_facts(FactMask::default(), false)
    };
    let before = facts.clone();
    let report = super::details_report(&facts);
    for diagnostic in [&older, &newest] {
        assert!(report.contains(diagnostic.label()), "{report}");
        assert!(report.contains(&diagnostic.words()), "{report}");
        assert!(
            report.contains(&aterm_agent::harness::usage::rfc3339_utc(
                diagnostic.completed_at
            )),
            "{report}"
        );
    }
    assert_eq!(facts, before);
    let card = shown(&rows(Fact::Account, &facts)).join("\n");
    assert!(!card.contains(&newest.words()), "{card}");
    assert!(!card.contains("Last local sign-in"), "{card}");
}

fn mask(kinds: &[FactKind]) -> FactMask {
    kinds
        .iter()
        .fold(FactMask::default(), |m, k| m.with(*k, true))
}

fn menu_facts(off: FactMask, side: bool) -> FactMenuFacts {
    FactMenuFacts {
        account_progress: None,
        account_queue: None,
        manual_signin_unavailable: false,
        title: "Folder \u{b7} tab 1".into(),
        name: None,
        room: 44,
        spoken: "Folder, tab 1".into(),
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        path: Some("~/aterm".into()),
        branch: Some("main".into()),
        context: Some("ctx 84% left".into()),
        own: None,
        weekly_gap: None,
        cli: None,
        refresh_usage: false,
        accounts: Vec::new(),
        accounts_refused: None,
        accounts_loading: false,
        off,
        starts_off: FactMask::default(),
        cwd: side.then(|| PathBuf::from("/Users//someone/aterm")),
        repo_root: side.then(|| PathBuf::from("/Users//someone")),
        git_hygiene: Vec::new(),
        folder_hygiene: Vec::new(),
        key_row: Some(true),
        note: None,
        typing: false,
        picks: None,
        stops: None,
        token: None,
        outlook: None,
        account_row: None,
        movable: true,
        product: "Claude Code",
        tabs_here: Some(1),
        ..FactMenuFacts::default()
    }
}

/// What automatic switching does for the tab is one sentence from the session's own recovery
/// observation, whatever this Mac's pin: pinning changes the `Pin` fact and its item and no
/// other row. The observation's timing is in the sentence only as the next look, on the local
/// clock, and an unknown clock is never said as an epoch date.
#[test]
fn the_recovery_sentence_is_independent_of_the_pool_pin() {
    use aterm_agent::harness::account_progress::{AccountProgress, ProgressPhase};
    let mut progress = AccountProgress::unknown(100);
    progress.phase = ProgressPhase::Waiting;
    progress.details.push("synthetic recovery blocker".into());
    progress.next_at = Some(160);
    let mut facts = menu_facts(FactMask::default(), false);
    facts.cli = Some(aterm_agent::harness::accounts::Cli::Claude);
    facts.now = 100;
    facts.accounts = vec![AccountMenuRow {
        account: AccountLabel::new("main").unwrap(),
        name: "main@example.test".into(),
        trail: "61% left".into(),
        spoken: "main@example.test, 61 percent left".into(),
        active: true,
        enabled: true,
    }];
    facts.account_progress = Some(progress);
    let notes = |rows: &[MenuRow]| {
        rows.iter()
            .filter_map(|row| match row {
                MenuRow::Note { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let before = rows(Fact::Account, &facts);
    assert_eq!(
        notes(&before),
        ["At its limit. aterm looks again 00:02."],
        "the sentence; a detail is clipboard evidence"
    );
    assert!(before.iter().any(|row| matches!(
        row,
        MenuRow::Item {
            act: MenuAct::Fact(Fact::Account, FactAct::NetworkState),
            ..
        }
    )));
    facts.pinned = Some("main@example.test".into());
    let after = rows(Fact::Account, &facts);
    assert_eq!(notes(&before), notes(&after));
    assert_ne!(before, after, "the pin is said on its own rows");
    for at in [None, Some(0), Some(-1)] {
        facts.account_progress.as_mut().unwrap().next_at = at;
        assert_eq!(
            notes(&rows(Fact::Account, &facts)),
            ["At its limit."],
            "{at:?}"
        );
    }
}

/// COPY DETAILS PRESERVES FULL EVIDENCE: every recovery detail however many, the phase, and
/// both times on both clocks, with the observation unchanged; an unknown clock is never an
/// epoch date. The item is on every Claude Code card with an observation or an address, and on
/// no other.
#[test]
fn copy_details_preserves_full_recovery_evidence() {
    use aterm_agent::harness::account_progress::{AccountProgress, ProgressPhase};
    let mut progress = AccountProgress::unknown(100);
    progress.phase = ProgressPhase::SigningIn {
        label: "current".into(),
        route: Some("browser".into()),
    };
    progress.account = Some("current".into());
    progress.known = Some(2);
    progress.eligible = Some(0);
    progress.wall = Some("weekly".into());
    progress.next_at = Some(160);
    let detail = "current: no usable sign-in route; profile(none) mailbox(none configured) vault(no new credential) hosts(none configured)";
    progress.details = vec![detail.into(); 30];
    let mut facts = menu_facts(FactMask::default(), false);
    facts.cli = Some(aterm_agent::harness::accounts::Cli::Claude);
    facts.now = 200;
    facts.accounts = vec![AccountMenuRow {
        account: AccountLabel::new("current").unwrap(),
        name: "current@example.test".into(),
        trail: "not read".into(),
        spoken: "current@example.test, not read".into(),
        active: true,
        enabled: true,
    }];
    facts.account_progress = Some(progress.clone());
    let report = super::details_report(&facts);
    assert_eq!(report.matches(detail).count(), 30);
    for at in [100, 160] {
        assert!(report.contains(&aterm_agent::harness::usage::rfc3339_utc(at)));
    }
    assert!(
        report.contains(&aterm_agent::harness::footer::account_progress_phase(
            &progress
        ))
    );
    assert_eq!(facts.account_progress.as_ref(), Some(&progress));
    for at in [0, -1] {
        let mut unknown = AccountProgress::unknown(at);
        unknown.next_at = Some(at);
        facts.account_progress = Some(unknown);
        let report = super::details_report(&facts);
        assert!(
            !report.contains(&aterm_agent::harness::usage::rfc3339_utc(at)),
            "an unknown clock must not become an epoch date"
        );
    }
    let copy = |facts: &FactMenuFacts| {
        rows(Fact::Account, facts)
            .iter()
            .filter_map(|row| match row {
                MenuRow::Item {
                    act: MenuAct::Fact(Fact::Account, FactAct::CopyRecovery),
                    label,
                    enabled: true,
                    ..
                } => Some(label.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(copy(&facts), ["Copy details"]);
    facts.account_progress = None;
    assert!(copy(&facts).is_empty(), "no observation and no address");
    facts.email = Some("current@example.test".into());
    assert_eq!(copy(&facts), ["Copy details"]);
    facts.cli = Some(aterm_agent::harness::accounts::Cli::Codex);
    assert!(copy(&facts).is_empty(), "Claude Code's alone");
}

fn shown(rows: &[MenuRow]) -> Vec<String> {
    rows.iter()
        .map(|row| match row {
            MenuRow::Item {
                act,
                label,
                enabled,
                glyph,
                ..
            } => format!("{glyph} {label} [{}] {enabled}", act.key()),
            MenuRow::Header(t) => format!("header {t}"),
            MenuRow::Note { text, .. } => format!("note {text}"),
            MenuRow::Rule => "rule".into(),
            MenuRow::Stops { stops, at, .. } => format!(
                "slider {} at {at:?}",
                stops
                    .iter()
                    .map(|stop| format!("{} [{}] {}", stop.label, stop.act.key(), stop.enabled))
                    .collect::<Vec<_>>()
                    .join(" | ")
            ),
            other => format!("{other:?}"),
        })
        .collect()
}

/// EACH FACT'S CARD OFFERS ITS ROWS, AND HIDES ONLY WHAT A CLICK HIDES: the folder's, the
/// branch's and the context's cards carry their own rows and no `Hide`, since a click on them
/// does nothing; another program's model and effort, which a click hides, keep theirs.
#[test]
fn each_fact_menu_offers_its_rows_and_only_a_clickable_fact_its_hide() {
    let on = menu_facts(FactMask::default(), true);
    assert_eq!(
        shown(&rows(Fact::Path, &on)),
        [
            "header Folder \u{b7} tab 1",
            "note \u{2302}  ~/aterm",
            "+ New tab here [fact-menu/path/new-tab-here] true",
            "\u{29c9} Copy path [fact-menu/path/copy-path] true",
        ]
    );
    assert_eq!(
        shown(&rows(Fact::Branch, &on))[1..],
        [
            "note \u{2387}  main",
            "\u{29c9} Copy branch name [fact-menu/branch/copy-branch] true",
            "+ New tab at the repository root [fact-menu/branch/new-tab-at-root] true",
        ]
    );
    assert_eq!(
        shown(&rows(Fact::Model, &on))[1..],
        [
            "note \u{25c6}  Opus 5.5",
            "rule",
            "\u{25cb} Hide model [fact-menu/model/hide/model] true",
        ],
        "no Claude Code: nothing types `/model`, and the card hides and shows"
    );
    assert_eq!(
        shown(&rows(Fact::Effort, &on))[1..],
        [
            "note \u{25c9}  xhigh",
            "rule",
            "\u{25cb} Hide effort [fact-menu/effort/hide/effort] true",
        ],
        "the key's on/off row is the key's own card's"
    );
    assert_eq!(
        shown(&rows(Fact::Context, &on))[1..],
        [
            "note \u{25d0}  ctx 84% left",
            "\u{22ef} Compact context\u{2026}  /compact [fact-menu/context/type/compact] true",
        ]
    );
    let with_hygiene = FactMenuFacts {
        git_hygiene: vec![
            "\u{2191}2 ahead \u{b7} \u{2193}1 behind origin/main".into(),
            "\u{2387}  2 worktrees \u{b7} 1 unmerged (feat-a) \u{b7} 1 stale (old-done)".into(),
        ],
        folder_hygiene: vec![
            "\u{26c1}  2 worktrees \u{b7} worktrees 512.0 MiB \u{b7} build/temp 2.0 GiB \u{b7} 120.0 GiB free".into(),
        ],
        ..menu_facts(FactMask::default(), true)
    };
    assert_eq!(
        shown(&rows(Fact::Branch, &with_hygiene))[1..4],
        [
            "note \u{2387}  main",
            "note \u{2191}2 ahead \u{b7} \u{2193}1 behind origin/main",
            "note \u{2387}  2 worktrees \u{b7} 1 unmerged (feat-a) \u{b7} 1 stale (old-done)",
        ]
    );
    assert_eq!(
        shown(&rows(Fact::Path, &with_hygiene))[1..3],
        [
            "note \u{2302}  ~/aterm",
            "note \u{26c1}  2 worktrees \u{b7} worktrees 512.0 MiB \u{b7} build/temp 2.0 GiB \u{b7} 120.0 GiB free",
        ]
    );
    for fact in [Fact::Path, Fact::Branch, Fact::Usage] {
        assert!(
            !shown(&rows(fact, &on)).iter().any(|r| r.contains("/type/")),
            "{fact:?}"
        );
    }
    let off = menu_facts(FactMask::default(), false);
    assert_eq!(
        shown(&rows(Fact::Path, &off))[2..4],
        [
            "+ New tab here [fact-menu/path/new-tab-here] false",
            "\u{29c9} Copy path [fact-menu/path/copy-path] false",
        ]
    );
    assert!(
        !shown(&rows(Fact::Branch, &off))
            .iter()
            .any(|r| r.contains("repository root"))
    );
    assert!(rows(Fact::Usage, &on).iter().all(|r| !r.selectable()));
}

fn pick(
    family: crate::claude_facts::ModelFamily,
    label: &str,
    arg: Option<&str>,
    current: bool,
    note: Option<&str>,
) -> crate::claude_facts::ModelPick {
    crate::claude_facts::ModelPick {
        family,
        label: label.into(),
        arg: arg.map(|arg| crate::claude_slash::ModelArg::new(arg).expect("a `/model` argument")),
        current,
        note: note.map(str::to_owned),
    }
}

/// A CLAUDE CODE SESSION'S MODEL CARD IS ITS TWO CHOICES: each under its own key, the session's
/// own checked, the Fable choice's billing line under it, and no hide or show row.
#[test]
fn the_model_card_is_its_two_choices_and_nothing_else() {
    use crate::claude_facts::ModelFamily;
    let card = FactMenuFacts {
        picks: Some(vec![
            pick(
                ModelFamily::Fable,
                "Fable 5.1",
                Some("claude-fable-5-1"),
                false,
                Some("billed to usage credits on this plan"),
            ),
            pick(
                ModelFamily::Opus,
                "Opus 5.5",
                Some("claude-opus-5-5"),
                true,
                None,
            ),
        ]),
        off: mask(&[FactKind::Path]),
        ..menu_facts(FactMask::default(), true)
    };
    let model = rows(Fact::Model, &card);
    assert_eq!(
        shown(&model)[1..],
        [
            "note \u{25c6}  Opus 5.5",
            "\u{25c6} Fable 5.1 [fact-menu/model/set-model/fable] true",
            "note    billed to usage credits on this plan",
            "\u{25c6} Opus 5.5 [fact-menu/model/set-model/opus] true",
        ],
        "no hide row, and no row shows the hidden folder again"
    );
    let checked: Vec<bool> = model
        .iter()
        .filter_map(|row| match row {
            MenuRow::Item { checked, .. } => Some(*checked),
            _ => None,
        })
        .collect();
    assert_eq!(checked, [false, true], "the session's own");
    let Some(MenuRow::Note { spoken, .. }) = model.get(3) else {
        panic!("the billing line: {model:?}");
    };
    assert_eq!(spoken, "Fable 5.1: billed to usage credits on this plan");
    // A family the build does not offer is shown and cannot be chosen.
    let unoffered = FactMenuFacts {
        picks: Some(vec![pick(
            ModelFamily::Fable,
            "Fable",
            None,
            false,
            Some("not offered by this Claude Code build"),
        )]),
        ..menu_facts(FactMask::default(), true)
    };
    assert_eq!(
        shown_rows(Fact::Model, &unoffered)[2],
        "\u{25c6} Fable [fact-menu/model/set-model/fable] false"
    );
}

/// A CLAUDE CODE SESSION'S EFFORT CARD IS A SLIDER: one row of its stops, each a level under
/// its own key, the thumb on the level shown; live under a refusal's note and while a press is
/// in flight, where an item waits.
#[test]
fn the_effort_card_is_a_slider_of_its_stops() {
    let slider = |stops: &[&'static str], at| FactMenuFacts {
        stops: Some((stops.to_vec(), at)),
        ..menu_facts(FactMask::default(), true)
    };
    let all = "slider medium [fact-menu/effort/set-effort/medium] true | \
               high [fact-menu/effort/set-effort/high] true | \
               xhigh [fact-menu/effort/set-effort/xhigh] true";
    assert_eq!(
        shown_rows(Fact::Effort, &slider(&["medium", "high", "xhigh"], Some(2)))[1..],
        [
            "note \u{25c9}  xhigh".to_owned(),
            format!("{all} at Some(2)"),
            "rule".to_owned(),
            "\u{25cb} Hide effort [fact-menu/effort/hide/effort] true".to_owned(),
        ]
    );
    let off_stops = FactMenuFacts {
        effort: Some("max".into()),
        ..slider(&["medium", "high", "xhigh"], None)
    };
    assert_eq!(
        shown_rows(Fact::Effort, &off_stops)[1..3],
        ["note \u{25c9}  max".to_owned(), format!("{all} at None")],
        "a level that is no stop: no thumb, and the note says the level"
    );
    let waiting = FactMenuFacts {
        note: Some("Claude is working; click when the turn ends".into()),
        typing: true,
        ..slider(&["medium", "high"], Some(0))
    };
    assert_eq!(
        shown_rows(Fact::Effort, &waiting)[2..4],
        [
            "note Claude is working; click when the turn ends",
            "slider medium [fact-menu/effort/set-effort/medium] true | \
             high [fact-menu/effort/set-effort/high] true at Some(0)",
        ],
        "the slider stays live: its pick is said in place, or waits its turn"
    );
    assert_eq!(
        shown_rows(Fact::Effort, &slider(&[], None))[2],
        "note this model takes no effort level"
    );
    assert!(
        !shown_rows(Fact::TopEffort, &slider(&["medium"], Some(0)))
            .iter()
            .any(|r| r.starts_with("slider")),
        "the top rule's key's card has no slider"
    );
    let keyed = |fact| {
        shown_rows(fact, &slider(&["medium"], Some(0)))
            .iter()
            .any(|r| r.contains("/type/key-on]"))
    };
    assert!(keyed(Fact::TopEffort), "the key's card keeps its row");
    assert!(
        !keyed(Fact::Effort),
        "and the slider's card has no row that types another command"
    );
}

fn shown_rows(fact: Fact, facts: &FactMenuFacts) -> Vec<String> {
    shown(&rows(fact, facts))
}

#[test]
fn the_typed_rows_say_what_they_type_and_wait_under_a_note() {
    use crate::claude_facts::ModelFamily;
    let key = aterm_primer::CLAUDE_TOP_EFFORT_KEY;
    let tagged = FactMenuFacts {
        key_row: Some(false),
        ..menu_facts(FactMask::default(), true)
    };
    assert_eq!(
        shown(&rows(Fact::TopEffort, &tagged))[2],
        format!(
            "\u{25c8} Turn {key} off for this session [fact-menu/top-effort/type/key-off] true"
        )
    );
    let picks = Some(vec![pick(
        ModelFamily::Opus,
        "Opus 5.5",
        Some("claude-opus-5-5"),
        true,
        None,
    )]);
    let noted = FactMenuFacts {
        note: Some("Claude is working; click when the turn ends".into()),
        picks: picks.clone(),
        ..menu_facts(FactMask::default(), true)
    };
    assert_eq!(
        shown(&rows(Fact::Model, &noted))[1..4],
        [
            "note \u{25c6}  Opus 5.5",
            "note Claude is working; click when the turn ends",
            "\u{25c6} Opus 5.5 [fact-menu/model/set-model/opus] false",
        ]
    );
    let typing = FactMenuFacts {
        typing: true,
        picks,
        ..menu_facts(FactMask::default(), true)
    };
    assert!(shown(&rows(Fact::Model, &typing))[2].ends_with("set-model/opus] false"));
    let top = shown(&rows(Fact::TopEffort, &typing));
    assert!(top[2].ends_with("[fact-menu/top-effort/type/key-on] false"));
    let effort = shown(&rows(Fact::Effort, &typing));
    assert!(
        effort
            .iter()
            .any(|r| r.ends_with("[fact-menu/effort/hide/effort] true")),
        "a row that types nothing does not wait: {effort:?}"
    );
    let context = shown(&rows(Fact::Context, &typing));
    assert!(context[2].ends_with("[fact-menu/context/type/compact] false"));
    let unkeyed = FactMenuFacts {
        key_row: None,
        ..menu_facts(FactMask::default(), true)
    };
    assert!(
        !shown(&rows(Fact::TopEffort, &unkeyed))
            .iter()
            .any(|r| r.contains(key))
    );
}

/// A CARD THAT HIDES SHOWS EVERY HIDDEN FACT AGAIN, AND A STARTING STUB'S CARD ITS OWN: the
/// model's card (no Claude Code) ends in its own `Hide` and a `Show` for each other hidden
/// fact. The folder's card carries neither in a program that shows the folder from the start;
/// where the program's own line prints the folder and the rule starts with it behind a stub
/// (`starts_off`), its card shows it again, or puts it back, and names no other fact.
#[test]
fn a_menu_shows_every_hidden_fact_again() {
    let hid = menu_facts(mask(&[FactKind::Path, FactKind::Branch]), true);
    let model = shown(&rows(Fact::Model, &hid));
    assert_eq!(
        model[model.len() - 4..],
        [
            "rule",
            "\u{25cb} Hide model [fact-menu/model/hide/model] true",
            "\u{25cf} Show folder [fact-menu/model/show/path] true",
            "\u{25cf} Show branch [fact-menu/model/show/branch] true",
        ]
    );
    let folder = shown(&rows(Fact::Path, &hid));
    assert!(
        !folder
            .iter()
            .any(|row| row.contains("Hide") || row.contains("Show")),
        "a folder shown from the start: no hide or show row: {folder:?}"
    );
    let starts = FactMenuFacts {
        starts_off: mask(&[FactKind::Path]),
        ..menu_facts(mask(&[FactKind::Path, FactKind::Branch]), true)
    };
    let folder = shown(&rows(Fact::Path, &starts));
    assert_eq!(
        folder[folder.len() - 2..],
        [
            "rule",
            "\u{25cf} Show folder [fact-menu/path/show/path] true",
        ],
        "a starting stub's card shows its own fact, and no other"
    );
    let back = FactMenuFacts {
        starts_off: mask(&[FactKind::Path]),
        ..menu_facts(FactMask::default(), true)
    };
    assert_eq!(
        shown(&rows(Fact::Path, &back)).last().map(String::as_str),
        Some("\u{25cb} Hide folder [fact-menu/path/hide/path] true"),
        "shown, it goes back behind its stub"
    );
    let branch = shown(&rows(Fact::Branch, &starts));
    assert!(
        !branch
            .iter()
            .any(|row| row.contains("Hide") || row.contains("Show")),
        "the branch starts shown there: {branch:?}"
    );
}

const T0: i64 = 86_400 * 19_800 + 14 * 3600 + 2 * 60;

fn traffic() -> TrafficView {
    TrafficView {
        start_s: T0,
        width_s: 60,
        buckets: vec![(1000, 10), (0, 0), (4000, 40), (2000, 0)].into(),
        busy: vec![(T0 + 120, T0 + 180)].into(),
        runtime_s: Some(393),
        unread: 0,
        totals: (20_000_000, 82_000),
    }
}

fn texts(rows: &[MenuRow]) -> Vec<String> {
    rows.iter()
        .map(|row| match row {
            MenuRow::Plot { cells, .. } => cells.iter().map(|c| c.ch).collect(),
            MenuRow::Note { text, .. } => format!("note {text}"),
            other => format!("{other:?}"),
        })
        .collect()
}

#[test]
fn the_traffic_card_paints_two_bands_and_its_axis() {
    let rows = traffic_rows(&traffic(), T0 + 240, Some(0), 30);
    let text = texts(&rows);
    assert_eq!(text.len(), 7, "{text:#?}");
    assert!(
        rows[..5]
            .iter()
            .all(|r| matches!(r, MenuRow::Plot { cells: c, .. } if c.len() == 30)),
        "{text:#?}"
    );
    assert!(text[0].starts_with("\u{2191} in  "), "{:?}", text[0]);
    assert!(text[1].starts_with("      "), "{:?}", text[1]);
    assert!(text[2].starts_with("\u{2193} out "), "{:?}", text[2]);
    assert!(text[3].starts_with("      "), "{:?}", text[3]);
    assert_eq!(text[4], "      14:02    4m00s       now");
    assert_eq!(
        text[5..],
        [
            "note 6m33s working \u{b7} \u{2191}20M in \u{b7} \u{2193}82k out",
            "note peak \u{2191}667 \u{b7} \u{2193}7 per 10s",
        ]
    );
    let ink = |row: usize| match &rows[row] {
        MenuRow::Plot { cells, .. } => cells[10].ink,
        _ => unreachable!("a graph row"),
    };
    assert_eq!(
        (ink(0), ink(2), ink(4)),
        (PlotInk::In, PlotInk::Out, PlotInk::Axis)
    );
    for (row, line) in text.iter().enumerate().take(4) {
        assert_eq!(line.chars().nth(6 + 7), Some(' '), "row {row}");
    }
    let unread = TrafficView {
        unread: 3,
        ..traffic()
    };
    assert_eq!(
        texts(&traffic_rows(&unread, T0 + 240, Some(0), 30)).last(),
        Some(&"note +3 unread not graphed".to_string())
    );
    assert_eq!(
        texts(&traffic_rows(&TrafficView::default(), T0, Some(0), 30)),
        ["note No traffic yet"]
    );
}

#[test]
fn a_band_peak_fills_both_rows_and_a_trickle_is_at_least_one_eighth() {
    assert_eq!(band_level(667, 667), 16);
    assert_eq!(band_glyphs(16), ('\u{2588}', '\u{2588}'));
    assert_eq!(band_level(1, 1_000_000), 1, "a trickle");
    assert_eq!(band_glyphs(1), (' ', '\u{2581}'));
    assert_eq!(band_glyphs(8), (' ', '\u{2588}'));
    assert_eq!(band_glyphs(9), ('\u{2581}', '\u{2588}'));
    assert_eq!(band_level(166, 667), 4);
    assert_eq!(band_glyphs(4), (' ', '\u{2584}'));
    assert_eq!(band_level(0, 667), 0);
    assert_eq!(band_glyphs(0), (' ', ' '));
    let text = texts(&traffic_rows(&traffic(), T0 + 240, Some(0), 30));
    let at = |row: usize, col: usize| text[row].chars().nth(6 + col);
    assert_eq!((at(0, 12), at(1, 12)), (Some('\u{2588}'), Some('\u{2588}')));
    assert_eq!((at(0, 0), at(1, 0)), (Some(' '), Some('\u{2584}')));
}

#[test]
fn busy_columns_take_the_shaded_ground() {
    use crate::autopilot_menu::{paint, place};
    let theme = aterm_render::Theme {
        fg: 0x00E0_E0E0,
        bg: 0x0018_1818,
        cursor: 0x00FF_FFFF,
        selection: 0x0044_4466,
    };
    let c = crate::tab_menu::menu_colors(theme);
    let mut card = vec![MenuRow::Header("Traffic \u{b7} tab 1".into())];
    card.extend(traffic_rows(&traffic(), T0 + 240, Some(0), 30));
    let rect = place(&card, (20, 0), (0..20, 0..80), (80, 21), 0).expect("a card");
    let painted = paint(rect, &card, Default::default(), None, theme, None);
    let row = &painted[3];
    let text = row.iter().map(|cell| cell.ch).collect::<String>();
    let lead = text
        .chars()
        .position(|ch| ch == '\u{2584}')
        .expect("the first minute's column");
    let busy = &row[lead + 12];
    assert_eq!(busy.ch, '\u{2588}', "{text:?}");
    assert_eq!(
        busy.bg,
        crate::chrome_band::mix3(c.card_bg, c.band.accent, 0.10)
    );
    assert_eq!(busy.fg, c.band.accent);
    assert_eq!(row[lead].bg, c.card_bg);
}

/// THE ACCOUNT CARD SAYS WHY THE WEEK IS MISSING, in its Budget answer: `not read:` and the
/// footer's own short reason, never a figure and never `none`; a reader hears it as the chip
/// speaks it. The full why is clipboard evidence (Copy details), whole. A figure, where there
/// is one, is the answer instead.
#[test]
fn the_account_menu_says_why_the_week_is_missing() {
    use aterm_agent::harness::profile::WeeklyGap;
    use aterm_agent::harness::tap::Refusal;
    for gap in [
        WeeklyGap::TapOff,
        WeeklyGap::Unoffered(Refusal::Exe),
        WeeklyGap::Unoffered(Refusal::Home),
        WeeklyGap::Unoffered(Refusal::Login),
        WeeklyGap::Unoffered(Refusal::Line),
        WeeklyGap::Unoffered(Refusal::Argv),
        WeeklyGap::Unoffered(Refusal::Settings),
        WeeklyGap::Unoffered(Refusal::Room),
        WeeklyGap::Untapped,
        WeeklyGap::Unrestarted,
        WeeklyGap::NoReply,
        WeeklyGap::Stale,
        WeeklyGap::ClockInvalid,
    ] {
        let facts = FactMenuFacts {
            title: "Account \u{b7} tab 1".into(),
            cli: Some(aterm_agent::harness::accounts::Cli::Claude),
            weekly_gap: Some(gap),
            room: 60,
            ..menu_facts(FactMask::default(), true)
        };
        let reason = gap.words().rsplit(" \u{b7} ").next().unwrap();
        let budget = |facts: &FactMenuFacts| {
            rows(Fact::Account, facts)
                .into_iter()
                .find_map(|row| match row {
                    MenuRow::Fact {
                        label,
                        value,
                        spoken,
                        ..
                    } if label == "Budget" => Some((value, spoken)),
                    _ => None,
                })
                .expect("the Budget answer")
        };
        assert_eq!(
            budget(&facts),
            (
                format!("not read: {reason}"),
                format!("Budget: {}", gap.spoken())
            ),
            "{gap:?}"
        );
        assert!(!reason.contains('%') && !reason.is_empty(), "{gap:?}");
        let whole = weekly_gap_why(gap).join(" ");
        assert!(super::details_report(&facts).contains(&whole), "{gap:?}");
        let metered = FactMenuFacts {
            own: Some(Figure::Exact(380)),
            ..facts
        };
        assert_eq!(
            budget(&metered).0,
            "62% left",
            "a figure says the week instead"
        );
        assert!(!super::details_report(&metered).contains(&whole));
    }
}

/// THE ACCOUNT MENU TAKES THE FOOTER'S WEEK as the footer says it: a figure's words (`weekly 39%
/// used`, a bound `weekly ≤62% left`) give the card's weekly words as said and the session's row
/// the same figure used (exact, or the bound), the session's own figure read past fifteen minutes
/// a lower bound, a 7d wall gives `weekly out` (never a percentage) and the session's row
/// [`Figure::Out`], and with neither the footer's reason goes to the menu's why.
#[test]
fn the_account_menu_takes_the_footers_week_or_its_reason() {
    use aterm_agent::harness::profile::{SubscriptionDisplay, WeeklyFigure, WeeklyGap};
    const NOW: i64 = 1_790_000_000;
    let sub = |weekly: Option<&str>, gap: Option<WeeklyGap>| SubscriptionDisplay {
        weekly: weekly.map(str::to_owned),
        weekly_gap: gap,
        ..SubscriptionDisplay::default()
    };
    assert_eq!(
        weekly_facts(&sub(Some("weekly 61% left"), None), false, NOW),
        (
            Some(Figure::Exact(390)),
            Some("weekly 61% left".into()),
            None
        ),
        "the words are said as they are and give the row the figure they say"
    );
    assert_eq!(
        weekly_facts(&sub(Some("weekly \u{2264}62% left"), None), false, NOW).0,
        Some(Figure::AtLeast(380)),
        "an aged figure's words with no figure behind them: the bound they say"
    );
    assert_eq!(
        weekly_facts(&sub(Some("weekly 0% left"), None), false, NOW).0,
        Some(Figure::Exact(1000)),
        "a reading's 100, said as read"
    );
    let aged = SubscriptionDisplay {
        weekly_figure: Some(WeeklyFigure {
            used_x10: 380,
            resets_at: NOW + 86_400,
            at: NOW - 40 * 60,
        }),
        ..sub(Some("weekly \u{2264}62% left"), None)
    };
    assert_eq!(
        weekly_facts(&aged, false, NOW).0,
        Some(Figure::AtLeast(380)),
        "the session's own aged figure is a lower bound in its row, never an age"
    );
    assert_eq!(
        weekly_facts(&aged, false, NOW - 30 * 60).0,
        Some(Figure::Exact(380)),
        "the control: the same figure while current"
    );
    assert_eq!(
        weekly_facts(&aged, false, NOW + 86_400),
        (None, None, None),
        "its week reset since the footer's read: no row figure and no words of the expired week"
    );
    let future = SubscriptionDisplay {
        weekly_figure: Some(WeeklyFigure {
            at: NOW + 61,
            ..aged.weekly_figure.unwrap()
        }),
        ..aged.clone()
    };
    assert_eq!(weekly_facts(&future, false, NOW), (None, None, None));
    assert_eq!(weekly_facts(&future, true, NOW).0, Some(Figure::Out));
    assert_eq!(
        weekly_facts(&aged, true, NOW + 86_400).0,
        Some(Figure::Out),
        "a 7d wall still says its own"
    );
    assert_eq!(
        weekly_facts(&sub(None, Some(WeeklyGap::NoReply)), false, NOW),
        (None, None, Some(WeeklyGap::NoReply)),
        "the footer's reason"
    );
    assert_eq!(
        weekly_facts(&sub(None, Some(WeeklyGap::NoReply)), true, NOW),
        (Some(Figure::Out), Some("weekly out".into()), None),
        "a 7d wall over every reason"
    );
}

/// AN ACCOUNT ROW SAYS ITS AGED STORE FIGURE AS A LOWER BOUND (the footer's one age rule,
/// `docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §2.1, said as what is left, `harness::left`):
/// exact for fifteen minutes (`61% left`), then, however old, a bound of use rounded down
/// (`≤62% left`) while its week runs, since use only rises within a week; never an old figure
/// read as exact, nor an inferred `0% left` for an account nobody read, nor an age. Each source is dated by its own reading: a tap's or a
/// sample's beyond the clock's skew is not one.
#[test]
fn an_account_rows_aged_store_figure_is_a_lower_bound() {
    use aterm_agent::harness::accounts;
    const NOW: i64 = 1_790_000_000;
    let roster = accounts::Roster {
        rows: vec![accounts::Row {
            label: "main".into(),
            class: accounts::Class::Preferred,
            dir: None,
            ..accounts::Row::default()
        }],
    };
    let enrolled = std::iter::once("main".to_string()).collect();
    let label_with = |at: i64, sample: Option<accounts::Sample>| {
        let mut state = accounts::State::default();
        let mut acct = accounts::Account {
            sample,
            ..accounts::Account::default()
        };
        acct.windows.insert(
            "seven_day".into(),
            accounts::WindowFig {
                pct_x10: 385,
                reset: NOW + 86_400,
                at,
                by: "m22".into(),
                source: accounts::SOURCE_TAP.into(),
                epoch_at: None,
            },
        );
        state.accounts.insert("main".into(), acct);
        let state = accounts::State::from_json(&state.to_json(), NOW).unwrap();
        assert_eq!(state.accounts["main"].windows["seven_day"].at, at);
        let rows = account_menu_rows_from(
            &roster,
            &state,
            &enrolled,
            true,
            None,
            None,
            SessionWeek::default(),
            NOW,
        );
        rows[0].standing().clone()
    };
    let label = |at: i64| label_with(at, None);
    assert_eq!(label(NOW - 60), "main \u{b7} 61% left");
    assert_eq!(label(NOW + 60), label(NOW - 60));
    let mut observed = accounts::State::default();
    accounts::note_auth(&mut observed, "main", accounts::Auth::Ok, NOW);
    let unknown = account_menu_rows_from(
        &roster,
        &observed,
        &enrolled,
        true,
        None,
        None,
        SessionWeek::default(),
        NOW,
    )[0]
    .standing()
    .clone();
    assert_eq!(label(NOW + 61), unknown);
    assert_eq!(label(NOW + 120), unknown);
    assert_eq!(label(0), unknown);
    let sampled = |read_at| accounts::Sample {
        week_pct: Some(41),
        week_reset: Some(NOW + 86_400),
        week_read_at: Some(read_at),
        at: NOW + 120,
        ..accounts::Sample::default()
    };
    assert_eq!(
        label_with(NOW + 120, Some(sampled(NOW - 20 * 60))),
        "main \u{b7} \u{2264}59% left",
        "a tap beyond the skew is no reading: the sample's"
    );
    assert_eq!(
        label_with(NOW - 20 * 60, Some(sampled(NOW + 120))),
        label(NOW - 20 * 60),
        "a sample beyond the skew is no reading: the tap's"
    );
    for (age, what) in [
        (20 * 60, "twenty minutes"),
        (3 * 3600, "three hours"),
        (2 * 86_400, "a two-day-old reading whose week still runs"),
    ] {
        assert_eq!(
            label(NOW - age),
            "main \u{b7} \u{2264}62% left",
            "{what}: a bound rounded down, never `\u{2264}61% left`, and no age"
        );
    }
    let restamped = accounts::Sample {
        week_pct: Some(41),
        week_reset: Some(NOW + 86_400),
        week_read_at: Some(NOW - 3 * 3600),
        at: NOW - 60,
        ..accounts::Sample::default()
    };
    assert_eq!(
        label_with(NOW - 3 * 3600, Some(restamped)),
        "main \u{b7} \u{2264}62% left",
        "a tap's note re-stamps the week it holds: as old as the window's reading, never read now"
    );
    let rows = account_menu_rows_from(
        &roster,
        &accounts::State::default(),
        &enrolled,
        true,
        Some("main"),
        None,
        SessionWeek {
            label: None,
            figure: None,
            said: Some(Figure::AtLeast(380)),
        },
        NOW,
    );
    assert_eq!(
        rows[0].standing(),
        "main \u{b7} \u{2264}62% left",
        "no store figure: the session's own, its bound kept"
    );
    assert_eq!(
        rows[0].spoken, "main, at most 62 percent left, this tab's account",
        "a reader hears the bound in words"
    );
    let beside = |session: Option<&str>, at: i64| {
        let mut state = accounts::State::default();
        let mut acct = accounts::Account::default();
        acct.windows.insert(
            "seven_day".into(),
            accounts::WindowFig {
                pct_x10: 385,
                reset: NOW + 86_400,
                at: NOW - 2 * 86_400,
                by: "m22".into(),
                source: accounts::SOURCE_TAP.into(),
                epoch_at: None,
            },
        );
        state.accounts.insert("main".into(), acct);
        let figure = aterm_agent::harness::profile::WeeklyFigure {
            used_x10: 600,
            resets_at: NOW + 86_400,
            at,
        };
        let rows = account_menu_rows_from(
            &roster,
            &state,
            &enrolled,
            true,
            session,
            None,
            SessionWeek {
                label: None,
                figure: Some(figure),
                said: Some(Figure::Exact(600)),
            },
            NOW,
        );
        rows[0].standing().clone()
    };
    assert_eq!(
        beside(Some("main"), NOW - 60),
        "main \u{b7} 40% left",
        "the session's own figure, read since the store's, on its own account's row"
    );
    assert_eq!(
        beside(Some("main"), NOW - 3 * 86_400),
        "main \u{b7} \u{2264}62% left",
        "the store's, read since the session's"
    );
    assert_eq!(
        beside(Some("other"), NOW - 60),
        "main \u{b7} \u{2264}62% left",
        "the control: another account's session figure never reaches this row"
    );
    {
        let mut state = accounts::State::default();
        let acct = accounts::Account {
            warned_week: Some(accounts::WindowSaid {
                pct: Some(20),
                reset: Some(NOW + 86_400),
                at: NOW - 2 * 86_400,
            }),
            ..accounts::Account::default()
        };
        state.accounts.insert("main".into(), acct);
        let figure = aterm_agent::harness::profile::WeeklyFigure {
            used_x10: 600,
            resets_at: NOW + 86_400,
            at: NOW - 25 * 60,
        };
        let rows = account_menu_rows_from(
            &roster,
            &state,
            &enrolled,
            true,
            Some("main"),
            None,
            SessionWeek {
                label: None,
                figure: Some(figure),
                said: None,
            },
            NOW,
        );
        assert_eq!(
            rows[0].standing(),
            "main \u{b7} \u{2264}40% left",
            "the session's own figure wins over a warned_week when no store window figure exists"
        );
    }

    let roster = accounts::Roster {
        rows: vec![
            accounts::Row {
                label: "main".into(),
                email: Some("main@example.test".into()),
                dir: Some("/logins/main".into()),
                ..accounts::Row::default()
            },
            accounts::Row {
                label: "work".into(),
                email: Some("work@example.test".into()),
                dir: Some("/logins/work".into()),
                ..accounts::Row::default()
            },
        ],
    };
    let enrolled = roster.rows.iter().map(|row| row.label.clone()).collect();
    let menu = |label, email, with_store| {
        let mut state = accounts::State {
            active: Some("work".into()),
            ..accounts::State::default()
        };
        if with_store {
            for (label, used) in [("main", 385), ("work", 200)] {
                let mut acct = accounts::Account::default();
                acct.windows.insert(
                    accounts::WEEK_LIMIT.into(),
                    accounts::WindowFig {
                        pct_x10: used,
                        reset: NOW + 86_400,
                        at: NOW - 40 * 60,
                        by: "m22".into(),
                        source: accounts::SOURCE_TAP.into(),
                        epoch_at: None,
                    },
                );
                state.accounts.insert(label.into(), acct);
            }
        }
        account_menu_rows_from(
            &roster,
            &state,
            &enrolled,
            true,
            label,
            email,
            SessionWeek {
                label: None,
                figure: Some(aterm_agent::harness::profile::WeeklyFigure {
                    used_x10: 600,
                    resets_at: NOW + 86_400,
                    at: NOW - 60,
                }),
                said: Some(Figure::Exact(600)),
            },
            NOW,
        )
    };
    let rows = menu(None, Some("main@example.test"), true);
    assert!(rows[0].active && !rows[1].active);
    assert!(rows[0].standing().ends_with("\u{b7} 40% left"));
    assert!(
        rows[1].standing().ends_with("\u{b7} \u{2264}80% left"),
        "a custom-home session identified only by email cannot replace machine-active work's figure"
    );
    for (label, email) in [
        (None, None),
        (None, Some("unlisted@example.test")),
        (Some("work"), Some("main@example.test")),
    ] {
        let rows = menu(label, email, true);
        assert!(
            !rows[0].active && rows[1].active,
            "the machine's UI fallback"
        );
        assert!(rows[0].standing().ends_with("\u{b7} \u{2264}62% left"));
        assert!(
            rows[1].standing().ends_with("\u{b7} \u{2264}80% left"),
            "an unknown or contradictory session identity lends no figure to the active row"
        );
        let rows = menu(label, email, false);
        assert!(
            rows.iter().all(|row| !row.standing().contains('%')),
            "nor can its weekly words fill an unobserved active row"
        );
    }
    let rows = menu(Some("main"), None, true);
    assert!(
        rows[0].standing().ends_with("\u{b7} 40% left"),
        "its own explicit label"
    );
    let rows = menu(None, Some("main@example.test"), false);
    assert!(rows[0].standing().ends_with("\u{b7} 40% left"));
    assert!(
        !rows[1].standing().contains('%'),
        "only the email's own row takes its words"
    );
}

#[test]
fn quota_rows_keep_only_time_admitted_post_refill_fallbacks() {
    use aterm_agent::harness::{accounts, profile::WeeklyFigure};
    const NOW: i64 = 1_790_000_000;
    let roster = accounts::Roster {
        rows: vec![accounts::Row {
            label: "main".into(),
            ..Default::default()
        }],
    };
    let enrolled = std::collections::BTreeSet::from(["main".to_owned()]);
    let row = |state: &accounts::State, session, now| {
        account_menu_rows_from(
            &roster,
            state,
            &enrolled,
            true,
            Some("main"),
            None,
            session,
            now,
        )
        .remove(0)
    };
    let mut observed = accounts::State::default();
    accounts::note_auth(&mut observed, "main", accounts::Auth::Ok, NOW);
    let empty = row(&observed, SessionWeek::default(), NOW);
    for (sample_after, own_after) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut state = accounts::State::default();
        let account = state.accounts.entry("main".into()).or_default();
        account.windows.insert(
            accounts::WEEK_LIMIT.into(),
            accounts::WindowFig {
                pct_x10: 950,
                reset: NOW + 86400,
                at: NOW + 120,
                by: "peer".into(),
                source: accounts::SOURCE_TAP.into(),
                epoch_at: Some(NOW - 600),
            },
        );
        account.sample = Some(accounts::Sample {
            week_pct: Some(38),
            week_reset: Some(NOW + 86400),
            at: NOW + 120,
            week_read_at: Some(if sample_after { NOW - 300 } else { NOW - 1200 }),
            ..Default::default()
        });
        let state = accounts::State::from_json(&state.to_json(), NOW).unwrap();
        let before = state.clone();
        let figure = WeeklyFigure {
            used_x10: 800,
            resets_at: NOW + 86400,
            at: if own_after { NOW - 240 } else { NOW - 1200 },
        };
        let session = SessionWeek {
            label: Some("main".into()),
            figure: Some(figure),
            said: Some(Figure::Exact(800)),
        };
        let got = row(&state, session.clone(), NOW);
        if own_after {
            let mut control = state.clone();
            control.accounts.clear();
            assert_eq!(got, row(&control, session, NOW));
        } else if sample_after {
            assert_eq!(got, row(&state, SessionWeek::default(), NOW));
            assert_ne!(got.standing(), empty.standing());
        } else {
            assert_eq!(got.standing(), empty.standing());
        }
        assert_eq!(
            row(&state, SessionWeek::default(), 0).standing(),
            empty.standing()
        );
        assert_eq!(state, before);
    }
    let mut state = accounts::State::default();
    state.accounts.entry("main".into()).or_default().warned_week = Some(accounts::WindowSaid {
        pct: Some(95),
        reset: Some(NOW + 86400),
        at: NOW + 120,
    });
    assert_eq!(
        row(&state, SessionWeek::default(), NOW).standing(),
        empty.standing()
    );
    accounts::note_wall(
        &mut state,
        "main",
        accounts::WEEK_LIMIT,
        Some(NOW + 3600),
        NOW,
        NOW,
    );
    let walled = row(&state, SessionWeek::default(), NOW);
    state.accounts.get_mut("main").unwrap().warned_week = None;
    assert_eq!(row(&state, SessionWeek::default(), NOW), walled);
}

/// THE SESSION'S WEEK REACHES THE ROW ITS LOGIN IS JUDGED TO (`SubscriptionDisplay::label`),
/// on a one-row roster too, where the footer names no account; never one whose email
/// contradicts the session's profile, nor the store's active fallback.
#[test]
fn the_sessions_week_reaches_the_row_its_login_is_judged_to() {
    use aterm_agent::harness::accounts;
    const NOW: i64 = 1_790_000_000;
    let roster = accounts::Roster {
        rows: vec![accounts::Row {
            label: "main".into(),
            class: accounts::Class::Preferred,
            dir: None,
            email: Some("main@example.test".into()),
            identity_email: Some("canonical@example.test".into()),
            ..accounts::Row::default()
        }],
    };
    let enrolled = std::iter::once("main".to_string()).collect();
    let mut state = accounts::State {
        active: Some("main".into()),
        ..accounts::State::default()
    };
    let mut acct = accounts::Account::default();
    acct.windows.insert(
        accounts::WEEK_LIMIT.into(),
        accounts::WindowFig {
            pct_x10: 385,
            reset: NOW + 86_400,
            at: NOW - 30 * 60,
            by: "m22".into(),
            source: accounts::SOURCE_TAP.into(),
            epoch_at: None,
        },
    );
    state.accounts.insert("main".into(), acct);
    let row = |label: Option<&str>, email: Option<&str>| {
        let rows = account_menu_rows_from(
            &roster,
            &state,
            &enrolled,
            true,
            None,
            email,
            SessionWeek {
                label: label.map(str::to_owned),
                figure: Some(aterm_agent::harness::profile::WeeklyFigure {
                    used_x10: 900,
                    resets_at: NOW + 86_400,
                    at: NOW - 60,
                }),
                said: Some(Figure::Exact(900)),
            },
            NOW,
        );
        (rows[0].standing().clone(), rows[0].active)
    };
    assert_eq!(
        row(Some("main"), None),
        ("main@example.test \u{b7} 10% left".into(), true),
        "the row its login is judged to takes its newer figure, with no email to match"
    );
    let kept = (
        "main@example.test \u{b7} \u{2264}62% left".to_string(),
        true,
    );
    for label in [Some("main"), None] {
        assert_eq!(
            row(label, Some("canonical@example.test")),
            ("main@example.test \u{b7} 10% left".into(), true),
            "the provider identity selects the alias row's newer figure"
        );
        assert_eq!(
            row(label, Some("main@example.test")),
            kept,
            "the sign-in alias is not evidence of the provider identity"
        );
    }
    assert_eq!(
        row(Some("main"), Some("other@example.test")),
        kept,
        "a judged row whose email contradicts the session's profile keeps its own week"
    );
    assert_eq!(
        row(None, Some("other@example.test")),
        kept,
        "another account's session, the store's active row: the row keeps its own week"
    );
}

/// The card says the account, the week as its Budget verdict, and one item for each of this
/// Mac's accounts under their heading, each choosing its own account; the pin's item is never
/// checked.
#[test]
fn account_menu_shows_the_week_and_rotation_accounts() {
    let facts = FactMenuFacts {
        title: "Account \u{b7} tab 1".into(),
        spoken: "Account, tab 1".into(),
        email: Some("sol-m27@example.test".into()),
        plan: Some("Max 20x".into()),
        own: Some(Figure::Exact(1000)),
        cli: Some(aterm_agent::harness::accounts::Cli::Claude),
        accounts: vec![
            AccountMenuRow {
                account: AccountLabel::new("m27").unwrap(),
                name: "m27".into(),
                trail: "0% left".into(),
                spoken: "m27, 0 percent left".into(),
                active: true,
                enabled: true,
            },
            AccountMenuRow {
                account: AccountLabel::new("m27-2").unwrap(),
                name: "m27-2".into(),
                trail: "not signed in".into(),
                spoken: "m27-2, not signed in, choosing it signs it in".into(),
                active: false,
                enabled: true,
            },
            AccountMenuRow {
                account: AccountLabel::new("m27-3").unwrap(),
                name: "m27-3".into(),
                trail: "not signed in".into(),
                spoken: "m27-3, not signed in, choosing it signs it in".into(),
                active: false,
                enabled: true,
            },
        ],
        ..menu_facts(FactMask::default(), true)
    };
    let menu = rows(Fact::Account, &facts);
    let said = shown(&menu);
    for want in [
        "header \u{2386}  sol-m27@example.test",
        "Fact { label: \"Budget\", value: \"0% left\", spoken: \"Budget: 0 percent left\", tone: Value }",
        "Fact { label: \"Plan\", value: \"Max 20x\", spoken: \"Plan: Max 20x\", tone: Value }",
        "Section(\"Switch this Mac to\")",
    ] {
        assert!(said.contains(&want.to_owned()), "{want}: {said:#?}");
    }
    let actions: Vec<_> = menu
        .iter()
        .filter_map(|row| match row {
            MenuRow::Item {
                act: MenuAct::Fact(Fact::Account, act),
                enabled,
                checked,
                ..
            } => Some((*act, *enabled, *checked)),
            _ => None,
        })
        .collect();
    assert_eq!(
        actions,
        [
            (
                FactAct::UseAccount(AccountLabel::new("m27").unwrap()),
                true,
                true
            ),
            (
                FactAct::UseAccount(AccountLabel::new("m27-2").unwrap()),
                true,
                false
            ),
            (
                FactAct::UseAccount(AccountLabel::new("m27-3").unwrap()),
                true,
                false
            ),
            (FactAct::AutoRotate, true, false),
            (FactAct::CopyRecovery, true, false),
            (FactAct::AccountSettings, true, false),
        ]
    );

    use aterm_agent::harness::accounts;
    let chosen = menu
        .iter()
        .find_map(|row| match row {
            MenuRow::Item {
                act: MenuAct::Fact(Fact::Account, FactAct::UseAccount(account)),
                enabled: true,
                ..
            } => Some(*account),
            _ => None,
        })
        .expect("the first displayed account is selectable");
    let labels = ["m27", "m27-2", "m27-3"];
    let roster = accounts::Roster {
        rows: labels
            .iter()
            .map(|label| accounts::Row {
                label: (*label).to_string(),
                dir: Some(format!("/synthetic/{label}")),
                ..accounts::Row::default()
            })
            .collect(),
    };
    let enrolled = labels.iter().map(|label| (*label).to_string()).collect();
    const NOW: i64 = 1_790_000_000;
    let mut before = accounts::State::default();
    for label in labels {
        let _ = accounts::note_wall(
            &mut before,
            label,
            "five_hour",
            Some(NOW + 3600),
            NOW - 10,
            NOW,
        );
    }
    for reordered in [false, true] {
        let mut current = roster.clone();
        if reordered {
            current.rows.reverse();
        }
        let row = chosen
            .row(&current)
            .expect("the selected account remains listed");
        let mut state = before.clone();
        accounts::pin(&mut state, &current, &enrolled, &row.label, NOW).unwrap();
        assert_eq!(state.active.as_deref(), Some("m27"));
        assert!(state.pinned);
        assert!(state.accounts["m27"].walls.is_empty());
        for other in ["m27-2", "m27-3"] {
            assert_eq!(state.accounts[other], before.accounts[other]);
        }
    }
    let mut removed = roster;
    removed.rows.retain(|row| row.label != "m27");
    let mut state = before.clone();
    let result = chosen
        .row(&removed)
        .ok_or_else(|| "the selected account is no longer listed".to_string())
        .and_then(|row| accounts::pin(&mut state, &removed, &enrolled, &row.label, NOW));
    assert!(result.is_err());
    assert_eq!(state, before);
}

/// The Account menu's rows tell a row a choice signs in from a disabled one (a reader is told
/// that choosing it signs it in; the row itself says only its condition), and a walled row says
/// `out`, never a percentage (glass design §2.1: never an inferred 100%). Negative control: a
/// limited row or a footer's wall said with a percentage fails the walled row.
#[test]
fn account_menu_rows_from_distinguishes_auto_login_and_disabled_rows() {
    use aterm_agent::harness::accounts;
    const NOW: i64 = 1_790_000_000;
    let roster = accounts::Roster {
        rows: vec![
            accounts::Row {
                label: "m15-2".into(),
                class: accounts::Class::Preferred,
                dir: None,
                email: Some("m15-2@example.test".into()),
                ..accounts::Row::default()
            },
            accounts::Row {
                label: "m15-3".into(),
                class: accounts::Class::Preferred,
                dir: Some("/synthetic/m15-3".into()),
                email: Some("m15-3@example.test".into()),
                ..accounts::Row::default()
            },
            accounts::Row {
                label: "m15-4".into(),
                class: accounts::Class::Preferred,
                dir: Some("/synthetic/m15-4".into()),
                email: None,
                ..accounts::Row::default()
            },
            accounts::Row {
                label: "m15-5".into(),
                class: accounts::Class::Pool,
                dir: Some("/synthetic/m15-5".into()),
                email: Some("m15-5@example.test".into()),
                ..accounts::Row::default()
            },
            accounts::Row {
                label: "m15-6".into(),
                class: accounts::Class::Pool,
                dir: Some("/synthetic/m15-6".into()),
                email: None,
                ..accounts::Row::default()
            },
        ],
    };
    let enrolled = ["m15-2", "m15-5", "m15-6"]
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut state = accounts::State::default();
    let _ = accounts::note_wall(
        &mut state,
        "m15-2",
        "seven_day",
        Some(NOW + 86_400),
        NOW - 60,
        NOW,
    );
    let _ = accounts::note_auth(&mut state, "m15-5", accounts::Auth::SignedOut, NOW);
    let _ = accounts::note_auth(&mut state, "m15-6", accounts::Auth::SignedOut, NOW);

    let menu_rows = account_menu_rows_from(
        &roster,
        &state,
        &enrolled,
        true,
        Some("m15-2"),
        Some("m15-2@example.test"),
        SessionWeek {
            label: None,
            figure: None,
            said: Some(Figure::Out),
        },
        NOW,
    );
    assert_eq!(
        menu_rows
            .iter()
            .map(|row| row.account.as_str())
            .collect::<Vec<_>>(),
        ["m15-2", "m15-3", "m15-4", "m15-5", "m15-6"]
    );
    assert_eq!(
        menu_rows
            .iter()
            .map(|row| (row.active, row.enabled))
            .collect::<Vec<_>>(),
        [
            (true, true),
            (false, true),
            (false, false),
            (false, true),
            (false, false)
        ]
    );
    // A wall says `out`, never a percentage (never an inferred 100% used).
    assert!(
        menu_rows[0].standing().ends_with("\u{b7} out"),
        "{menu_rows:?}"
    );
    assert_eq!(
        menu_rows[0].spoken,
        "m15-2@example.test, out, this tab's account"
    );
    assert!(menu_rows.iter().all(|row| !row.standing().contains('%')));
    assert_eq!(
        menu_rows
            .iter()
            .map(|row| row.trail.as_str())
            .collect::<Vec<_>>(),
        [
            "out",
            "not signed in",
            "not signed in",
            "signed out",
            "signed out"
        ],
        "{menu_rows:?}"
    );
    assert_eq!(
        menu_rows
            .iter()
            .map(|row| row.spoken.ends_with(", choosing it signs it in"))
            .collect::<Vec<_>>(),
        [false, true, false, true, false],
        "{menu_rows:?}"
    );

    // Where no row signs in by itself (Grok Build's pool), a row with an email that is signed
    // out or not enrolled is as one with none: it names its condition and cannot be chosen.
    let menu_rows = account_menu_rows_from(
        &roster,
        &state,
        &enrolled,
        false,
        Some("m15-2"),
        Some("m15-2@example.test"),
        SessionWeek::default(),
        NOW,
    );
    assert_eq!(
        menu_rows.iter().map(|row| row.enabled).collect::<Vec<_>>(),
        [true, false, false, false, false]
    );
    assert!(
        menu_rows
            .iter()
            .all(|row| !row.spoken.contains("signs it in")),
        "{menu_rows:?}"
    );
}

/// A GROK SESSION'S ACCOUNT MENU OFFERS NO AUTOMATIC SIGN-IN: no Grok renewal starts in the
/// background (`Cli::renews_headlessly`), so a Grok row with an email that is not enrolled here
/// cannot be chosen, where choosing it would start nothing. CONTROL: the same row in Kimi's pool
/// can be chosen, its renewal starting by itself. Unix only, as the manual sign-in queue is
/// (`accounts::manual_signin_capability`): elsewhere no not-enrolled row can be chosen.
#[cfg(unix)]
#[test]
fn a_grok_pools_menu_offers_no_automatic_sign_in() {
    use aterm_agent::harness::accounts::Cli;
    const NOW: i64 = 1_792_660_000;
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-pool-menu-")
        .tempdir()
        .expect("tempdir");
    let root = dir.path();
    let (home, config, state) = (root.join("home"), root.join("config"), root.join("state"));
    for d in [&home, &config, &state] {
        std::fs::create_dir_all(d).unwrap();
    }
    let aterm_toml = config.join("aterm.toml");
    crate::test_scratch::write_config(&aterm_toml, "[fleet]\nmachine = \"m16\"\n");
    for (cli, extra) in [(Cli::Grok, ""), (Cli::Kimi, "region = \"global\"\n")] {
        let id = cli.id();
        let login = home.join(format!(".aterm/logins/{id}/{id}-1"));
        std::fs::write(
            config.join(format!("accounts-{id}.toml")),
            format!(
                "[[account]]\nlabel = \"{id}-1\"\nclass = \"dedicated\"\ndir = {}\n\
                 email = \"{id}-one@example.com\"\n{extra}",
                crate::test_host::toml(&login)
            ),
        )
        .unwrap();
        let rows = pool_menu_rows(
            cli,
            (&aterm_toml, &state, Some(home.as_path())),
            (None, None),
            SessionWeek::default(),
            NOW,
        )
        .unwrap();
        assert_eq!(rows.len(), 1, "{id}: {rows:?}");
        assert_eq!(rows[0].enabled, cli != Cli::Grok, "{id}: {rows:?}");
    }
}

/// A CODEX POOL'S DEFAULT ROW is signed in here only while `~/.codex/auth.json` is there: with
/// it gone the menu says the row is not enrolled and offers no switch to it, as a launch passes
/// it over (`accounts::enrolled_in`). CONTROL: with the file back the row is offered again.
#[test]
fn a_default_rows_menu_entry_follows_its_login_file() {
    use aterm_agent::harness::accounts::{Cli, PinSelection, Roster, Store};
    const NOW: i64 = 1_792_660_000;
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-pool-menu-default-")
        .tempdir()
        .expect("tempdir");
    let root = dir.path();
    let (home, config, state) = (root.join("home"), root.join("config"), root.join("state"));
    for d in [&home, &config, &state] {
        std::fs::create_dir_all(d).unwrap();
    }
    let aterm_toml = config.join("aterm.toml");
    crate::test_scratch::write_config(&aterm_toml, "[fleet]\nmachine = \"m16\"\n");
    let work = home.join(".aterm/logins/codex/work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("auth.json"), "{}").unwrap();
    std::fs::write(
        config.join("accounts-codex.toml"),
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n[[account]]\n\
             label = \"work\"\nclass = \"dedicated\"\ndir = {}\n",
            crate::test_host::toml(&work)
        ),
    )
    .unwrap();
    let main_row = || {
        let rows = pool_menu_rows(
            Cli::Codex,
            (&aterm_toml, &state, Some(home.as_path())),
            (None, None),
            SessionWeek::default(),
            NOW,
        )
        .unwrap();
        rows.into_iter()
            .find(|row| row.account.as_str() == "main")
            .expect("main's row")
    };
    // The menu's pin path agrees: no pin on a row a launch would refuse.
    let roster_path = config.join("accounts-codex.toml");
    let select = || {
        let roster = Roster::load_for(Cli::Codex, &roster_path).unwrap();
        Store::at_for(&state, Cli::Codex, &home).select_account_for(
            Cli::Codex,
            &roster,
            &roster_path,
            "main",
            NOW,
        )
    };
    let gone = main_row();
    assert_eq!(gone.trail, "not signed in", "{gone:?}");
    assert!(!gone.enabled, "{gone:?}");
    assert!(select().is_err(), "a row not signed in here is not pinned");
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::write(home.join(".codex/auth.json"), "{}").unwrap();
    let back = main_row();
    assert_ne!(back.trail, "not signed in", "{back:?}");
    assert!(back.enabled, "{back:?}");
    assert!(
        matches!(select(), Ok(PinSelection::Pinned)),
        "signed in again, it is pinned"
    );
}

/// A KIMI SESSION'S ACCOUNT MENU lists the rows of Kimi's own roster (`accounts-kimi.toml`),
/// never Claude Code's beside it, and an empty pool names `aterm harness accounts add --cli
/// kimi`. CONTROL: Claude Code's empty state is `No rotation accounts configured — run aterm
/// init`, as it always was, whether the facts name Claude Code or no CLI.
#[test]
fn a_kimi_sessions_account_menu_reads_its_own_pool() {
    use aterm_agent::harness::accounts::Cli;
    const NOW: i64 = 1_792_660_000;
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-pool-menu-")
        .tempdir()
        .expect("tempdir");
    let root = dir.path();
    let (home, config, state) = (root.join("home"), root.join("config"), root.join("state"));
    for d in [&home, &config, &state] {
        std::fs::create_dir_all(d).unwrap();
    }
    let aterm_toml = config.join("aterm.toml");
    crate::test_scratch::write_config(&aterm_toml, "[fleet]\nmachine = \"m16\"\n");
    std::fs::write(
        config.join("accounts.toml"),
        "[[account]]\nlabel = \"m16\"\nemail = \"claude-one@example.com\"\n",
    )
    .unwrap();
    let none = pool_menu_rows(
        Cli::Kimi,
        (&aterm_toml, &state, Some(home.as_path())),
        (None, None),
        SessionWeek::default(),
        NOW,
    )
    .unwrap();
    assert!(none.is_empty(), "no Kimi roster, no rows: {none:?}");
    let login = home.join(".aterm/logins/kimi/kimi-1");
    std::fs::write(
        config.join("accounts-kimi.toml"),
        format!(
            "[[account]]\nlabel = \"kimi-1\"\nclass = \"dedicated\"\ndir = '{}'\n\
             email = \"kimi-one@example.com\"\nregion = \"global\"\n",
            login.display()
        ),
    )
    .unwrap();
    let listed = pool_menu_rows(
        Cli::Kimi,
        (&aterm_toml, &state, Some(home.as_path())),
        (None, None),
        SessionWeek::default(),
        NOW,
    )
    .unwrap();
    let labels: Vec<&str> = listed.iter().map(|row| row.account.as_str()).collect();
    assert_eq!(labels, ["kimi-1"], "Kimi's roster alone: {listed:?}");

    for damaged in ["state", "logins"] {
        let dir = root.join(damaged);
        let store = aterm_agent::harness::accounts::Store::at_for(&dir, Cli::Kimi, &home);
        store.ensure().unwrap();
        let path = if damaged == "state" {
            store.state_file()
        } else {
            store.logins_file()
        };
        std::fs::write(&path, "{").unwrap();
        let why = pool_menu_rows(
            Cli::Kimi,
            (&aterm_toml, &dir, Some(home.as_path())),
            (None, None),
            SessionWeek::default(),
            NOW,
        )
        .unwrap_err();
        let refused = FactMenuFacts {
            cli: Some(Cli::Kimi),
            accounts_refused: Some(why),
            ..menu_facts(FactMask::default(), true)
        };
        let menu_rows = rows(Fact::Account, &refused);
        let menu = shown(&menu_rows);
        assert!(
            menu.iter()
                .any(|row| row.contains("This Mac's accounts could not be read:")),
            "{menu:?}"
        );
        assert!(
            !menu_rows.iter().any(|row| matches!(
                row,
                MenuRow::Item {
                    act: MenuAct::Fact(Fact::Account, FactAct::AutoRotate),
                    ..
                }
            )),
            "{menu:?}"
        );
        assert!(
            !menu.iter().any(|row| row.contains("accounts add")),
            "{menu:?}"
        );
        assert_eq!(std::fs::read(path).unwrap(), b"{");
    }

    let empty = FactMenuFacts {
        title: "Account \u{b7} tab 1".into(),
        cli: Some(Cli::Kimi),
        product: "Kimi",
        ..menu_facts(FactMask::default(), true)
    };
    let menu_rows = rows(Fact::Account, &empty);
    let menu = shown(&menu_rows);
    assert!(
        menu.contains(&"note No Kimi accounts are listed on this Mac.".to_string()),
        "{menu:?}"
    );
    assert!(
        menu.join(" ")
            .contains("note Add one: aterm harness accounts add --cli note kimi <label>"),
        "{menu:?}"
    );
    assert!(
        !menu.iter().any(|row| row.contains("aterm init"))
            && !menu_rows.iter().any(|row| matches!(
                row,
                MenuRow::Item {
                    act: MenuAct::Fact(Fact::Account, FactAct::AutoRotate),
                    ..
                }
            )),
        "{menu:?}"
    );
    let claude_empty = "note with aterm init.".to_string();
    let claude = FactMenuFacts {
        title: "Account \u{b7} tab 1".into(),
        cli: Some(Cli::Claude),
        ..menu_facts(FactMask::default(), true)
    };
    assert!(
        shown(&rows(Fact::Account, &claude)).contains(&claude_empty),
        "CONTROL: Claude Code's empty state"
    );
}

/// A MUSE SESSION'S ACCOUNT MENU SPEAKS FOR MUSE, not the rotation pool it has none of: its
/// own login note, its usage refresh under `⇄` (never `↻`, which reads as the reset's `↺`,
/// `docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §4.2), and never Claude Code's roster or
/// settings rows.
#[test]
fn a_muse_sessions_account_menu_is_its_own_login() {
    let facts = FactMenuFacts {
        title: "Account \u{b7} tab 1".into(),
        spoken: "Account, tab 1".into(),
        email: Some("someone@example.test".into()),
        plan: Some("max usage".into()),
        own: Some(Figure::Exact(490)),
        product: "Muse",
        refresh_usage: true,
        ..menu_facts(FactMask::default(), true)
    };
    let menu = shown(&rows(Fact::Account, &facts));
    assert!(
        menu.iter()
            .any(|row| row.contains("Muse keeps its own login")),
        "{menu:?}"
    );
    assert!(
        menu.iter()
            .any(|row| row.contains("Refresh usage") && row.contains("refresh-usage")),
        "{menu:?}"
    );
    assert!(rows(Fact::Account, &facts).iter().all(|row| match row {
        MenuRow::Item { act, glyph, .. } =>
            *act == MenuAct::Fact(Fact::Account, FactAct::RefreshUsage)
                && *glyph == crate::fact_menu::PIN_GLYPH,
        _ => true,
    }));
    let plain = FactMenuFacts {
        refresh_usage: false,
        ..facts
    };
    let menu = shown(&rows(Fact::Account, &plain));
    assert!(
        !menu.iter().any(|row| row.contains("Refresh usage")),
        "{menu:?}"
    );
}

// Unix only, as the manual sign-in queue a not-enrolled row's selection needs is
// (`accounts::manual_signin_capability`): elsewhere selecting one refuses.
#[cfg(unix)]
#[test]
fn select_menu_account_for_pins_enrolled_rows_and_dispatches_auto_login_per_cli_pool() {
    use aterm_agent::harness::accounts::{self, Cli};
    const NOW: i64 = 1_792_660_000;

    let tmp = aterm_tempfile::tempdir().unwrap();
    let (config, state, home) = (
        tmp.path().join("config"),
        tmp.path().join("state"),
        tmp.path().join("home"),
    );
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let login_1 = home.join(".aterm/logins/codex/codex-1");
    let login_2 = home.join(".aterm/logins/codex/codex-2");
    let login_3 = home.join(".aterm/logins/codex/codex-3");
    for d in [&login_1, &login_2, &login_3] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(login_1.join("auth.json"), "{}").unwrap();
    let roster_path = config.join("accounts-codex.toml");
    std::fs::write(
        &roster_path,
        format!(
            "[[account]]\nlabel = \"codex-1\"\nclass = \"dedicated\"\ndir = {}\n\
             email = \"codex-one@example.com\"\n\n\
             [[account]]\nlabel = \"codex-2\"\nclass = \"dedicated\"\ndir = {}\n\
             email = \"codex-two@example.com\"\n\n\
             [[account]]\nlabel = \"codex-3\"\nclass = \"dedicated\"\ndir = {}\n",
            crate::test_host::toml(&login_1),
            crate::test_host::toml(&login_2),
            crate::test_host::toml(&login_3),
        ),
    )
    .unwrap();
    let codex_store = accounts::Store::at_for(&state, Cli::Codex, &home);
    let claude_store = accounts::Store::at(&state);

    // 1. Enrolled vendor row pins immediately without dispatching a sign-in worker, and never
    // touches Claude Code's store.
    let mut dispatched = Vec::new();
    assert!(super::select_menu_account_for(
        Cli::Codex,
        (&roster_path, &state, Some(&home)),
        &AccountLabel::new("codex-1").unwrap(),
        NOW,
        true,
        |req, _| {
            dispatched.push(req);
            Ok(())
        },
    ));
    assert!(dispatched.is_empty());
    let (s1, _) = codex_store.load(NOW).unwrap();
    assert_eq!(s1.active.as_deref(), Some("codex-1"));
    assert!(s1.pinned);
    assert!(claude_store.load(NOW).unwrap().0.active.is_none());

    // 2. CONTROL: unenrolled vendor row under `Inert` upkeep (`upkeep_live = false`) cancels
    // `pending_pin` immediately and dispatches nothing.
    assert!(super::select_menu_account_for(
        Cli::Codex,
        (&roster_path, &state, Some(&home)),
        &AccountLabel::new("codex-2").unwrap(),
        NOW + 1,
        false,
        |req, _| {
            dispatched.push(req);
            Ok(())
        },
    ));
    assert!(dispatched.is_empty());
    assert!(codex_store.load(NOW + 1).unwrap().0.pending_pin.is_none());

    // 3. Unenrolled `(auto-login)` vendor row under `Live` upkeep saves `pending_pin` in Codex's
    // store and dispatches the exact `PinRequest`.
    assert!(super::select_menu_account_for(
        Cli::Codex,
        (&roster_path, &state, Some(&home)),
        &AccountLabel::new("codex-2").unwrap(),
        NOW + 2,
        true,
        |req, h| {
            assert_eq!(h, home);
            dispatched.push(req);
            Ok(())
        },
    ));
    assert_eq!(dispatched.len(), 1);
    assert_eq!(dispatched[0].label(), "codex-2");
    assert_eq!(
        codex_store
            .load(NOW + 2)
            .unwrap()
            .0
            .pending_pin
            .as_ref()
            .and_then(aterm_agent::harness::accounts::PendingSelection::account),
        Some(&dispatched[0])
    );

    // 4. CONTROL: dispatch failure cancels `pending_pin`; no-email row never saves one.
    assert!(super::select_menu_account_for(
        Cli::Codex,
        (&roster_path, &state, Some(&home)),
        &AccountLabel::new("codex-2").unwrap(),
        NOW + 3,
        true,
        |_, _| Err(()),
    ));
    assert!(codex_store.load(NOW + 3).unwrap().0.pending_pin.is_none());
    assert!(!super::select_menu_account_for(
        Cli::Codex,
        (&roster_path, &state, Some(&home)),
        &AccountLabel::new("codex-3").unwrap(),
        NOW + 4,
        true,
        |req, _| {
            dispatched.push(req);
            Ok(())
        },
    ));
    assert_eq!(dispatched.len(), 1);
    assert!(codex_store.load(NOW + 4).unwrap().0.pending_pin.is_none());

    let before = codex_store.load(NOW + 5).unwrap().0;
    std::fs::rename(codex_store.ledger_path(), state.join("retained-ledger")).unwrap();
    std::fs::create_dir(codex_store.ledger_path()).unwrap();
    assert!(!super::select_menu_account_for(
        Cli::Codex,
        (&roster_path, &state, Some(&home)),
        &AccountLabel::new("codex-1").unwrap(),
        NOW + 5,
        true,
        |_, _| panic!("a failed selection must not dispatch authentication"),
    ));
    assert_eq!(codex_store.load(NOW + 5).unwrap().0, before);
}

/// THE ACCOUNT MENU'S WHY NEVER ASKS THE PERSON FOR THE WEEK: no line under
/// the footer's reason names `/usage` or a command for the person to type; it says why the
/// harness has no figure.
#[test]
fn no_weekly_why_tells_the_person_to_open_usage() {
    use aterm_agent::harness::profile::WeeklyGap;
    use aterm_agent::harness::tap::Refusal;
    for gap in [
        WeeklyGap::TapOff,
        WeeklyGap::Unoffered(Refusal::Exe),
        WeeklyGap::Unoffered(Refusal::Home),
        WeeklyGap::Unoffered(Refusal::Login),
        WeeklyGap::Unoffered(Refusal::Line),
        WeeklyGap::Unoffered(Refusal::Argv),
        WeeklyGap::Unoffered(Refusal::Settings),
        WeeklyGap::Unoffered(Refusal::Room),
        WeeklyGap::Untapped,
        WeeklyGap::Unrestarted,
        WeeklyGap::NoReply,
        WeeklyGap::Stale,
    ] {
        let whole = weekly_gap_why(gap).join(" ");
        for asks in ["/usage", "open ", "aterm ctl"] {
            assert!(!whole.contains(asks), "{gap:?}: {whole:?} asks {asks:?}");
        }
    }
}

/// T8 (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md`): AN ACCOUNT ROW WITH NO FIGURE
/// SHOWS NO NUMBER, NEVER AN INFERRED ONE (an unread week as `0% used`, as `100% left` was):
/// an account nobody read here (`Unseen`), one whose login is not made here but signs in by
/// itself (`NotEnrolled` with an email), and one on record with no figure of its week (`Usable`,
/// its login proven only) may each be spent elsewhere, so each says `not read`, or that it is not
/// signed in, and a reader is told the one a choice signs in. Negative control: an unread week
/// given a figure (the old `100% left` fallback) restored.
#[test]
fn no_row_reads_full_without_a_figure() {
    use aterm_agent::harness::accounts;
    const NOW: i64 = 1_790_000_000;
    let row = |label: &str, dir: Option<&str>, email: Option<&str>| accounts::Row {
        label: label.into(),
        class: accounts::Class::Preferred,
        dir: dir.map(str::to_owned),
        email: email.map(str::to_owned),
        ..accounts::Row::default()
    };
    let roster = accounts::Roster {
        rows: vec![
            row("unseen", None, None),
            row("away", Some("/nonexistent/alt1"), Some("alt1@example.test")),
            row("proven", None, None),
        ],
    };
    let enrolled = ["unseen", "proven"]
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut state = accounts::State::default();
    let _ = accounts::note_auth(&mut state, "proven", accounts::Auth::Ok, NOW - 60);
    let conditions: Vec<_> = roster
        .rows
        .iter()
        .map(|r| accounts::condition(r, &state, r.dir.is_none() || r.label != "away", NOW).as_str())
        .collect();
    assert_eq!(conditions, ["unseen", "not-enrolled", "usable"]);
    let rows = account_menu_rows_from(
        &roster,
        &state,
        &enrolled,
        true,
        None,
        None,
        SessionWeek::default(),
        NOW,
    );
    let trails: Vec<&str> = rows.iter().map(|row| row.trail.as_str()).collect();
    assert_eq!(trails, ["not read", "not signed in", "not read"]);
    for row in &rows {
        assert!(
            !row.standing()
                .contains(|c: char| c == '%' || c.is_ascii_digit())
                || row.name.contains(|c: char| c.is_ascii_digit()),
            "no number: {row:?}"
        );
        assert!(!row.spoken.contains("percent"), "{row:?}");
    }
    let signs_in: Vec<bool> = rows
        .iter()
        .map(|row| row.spoken.ends_with(", choosing it signs it in"))
        .collect();
    assert_eq!(signs_in, [false, true, false]);
}

/// EVERY GLASS WEEK FIGURE SAYS LEFT (owner, 2026-11-09, asked "used or left": "left
/// everywhere"; `harness::left`). Over Account menu rows of every condition and figure (a wall,
/// signed out and not signed in each with and without a sign-in a choice starts, a fresh
/// reading, an aged one, one of 99.5% used and one of 100%, a warned week, and none), each
/// trail says what is left of its week (`61% left`, an older reading's bound `≤62% left`, a
/// wall's `out`, `0% left` only where a reading said 100) or the condition that stands before
/// any figure (`signed out`, `not signed in`, `not read`), each spoken row says it in words
/// (`at most 62 percent left`), and no row says `used`. Negative control: the rows' words of 2026-11-07 to 2026-11-09
/// (`row_figure`'s `39% used`, an aged one's `≥38% used`, a read 100's `100% used (limited)`)
/// fail it.
#[test]
fn every_glass_week_figure_says_left() {
    use aterm_agent::harness::accounts;
    const NOW: i64 = 1_790_000_000;
    let row = |label: &str, dir: &str, email: bool| accounts::Row {
        label: label.into(),
        class: accounts::Class::Pool,
        dir: Some(format!("/synthetic/{dir}")),
        email: email.then(|| format!("{label}@example.test")),
        ..accounts::Row::default()
    };
    let roster = accounts::Roster {
        rows: vec![
            row("walled", "walled", true),
            row("fresh", "fresh", false),
            row("aged", "aged", false),
            row("nearly", "nearly", false),
            row("spent", "spent", false),
            row("warned", "warned", false),
            row("gone", "gone", true),
            row("gone-quiet", "gone-quiet", false),
            row("away", "away", true),
            row("away-quiet", "away-quiet", false),
            row("away-unread", "away-unread", true),
            row("unread", "unread", false),
        ],
    };
    let enrolled = roster
        .rows
        .iter()
        .map(|row| row.label.clone())
        .filter(|label| !label.starts_with("away"))
        .collect();
    let mut state = accounts::State::default();
    for (label, pct_x10, at) in [
        ("fresh", 385, NOW - 60),
        ("aged", 385, NOW - 3 * 3600),
        ("nearly", 995, NOW - 60),
        ("spent", 1000, NOW - 60),
        ("gone", 200, NOW - 60),
        ("away", 500, NOW - 3 * 3600),
    ] {
        state
            .accounts
            .entry(label.into())
            .or_default()
            .windows
            .insert(
                accounts::WEEK_LIMIT.into(),
                accounts::WindowFig {
                    pct_x10,
                    reset: NOW + 86_400,
                    at,
                    by: "m22".into(),
                    source: accounts::SOURCE_TAP.into(),
                    epoch_at: None,
                },
            );
    }
    state
        .accounts
        .entry("warned".into())
        .or_default()
        .warned_week = Some(accounts::WindowSaid {
        pct: Some(70),
        reset: Some(NOW + 86_400),
        at: NOW - 60,
    });
    let _ = accounts::note_wall(
        &mut state,
        "walled",
        accounts::WEEK_LIMIT,
        Some(NOW + 86_400),
        NOW - 60,
        NOW,
    );
    for label in ["gone", "gone-quiet"] {
        let _ = accounts::note_auth(&mut state, label, accounts::Auth::SignedOut, NOW);
    }
    let _ = accounts::note_auth(&mut state, "unread", accounts::Auth::Ok, NOW);
    let words = |auto_login: bool| {
        account_menu_rows_from(
            &roster,
            &state,
            &enrolled,
            auto_login,
            None,
            None,
            SessionWeek::default(),
            NOW,
        )
        .into_iter()
        .map(|row| (row.name.clone(), row.trail.clone(), row.spoken))
        .collect::<Vec<_>>()
    };
    // A row with an email is named by it (§2.5), one with none by its roster label.
    let said = |label: &str, week: &str, spoken: &str| {
        let name = if ["walled", "gone", "away", "away-unread"].contains(&label) {
            format!("{label}@example.test")
        } else {
            label.to_owned()
        };
        (name.clone(), week.to_owned(), format!("{name}, {spoken}"))
    };
    assert_eq!(
        words(true),
        [
            said("walled", "out", "out"),
            said("fresh", "61% left", "61 percent left"),
            said("aged", "\u{2264}62% left", "at most 62 percent left"),
            said("nearly", "1% left", "1 percent left"),
            said("spent", "0% left", "0 percent left"),
            said("warned", "30% left", "30 percent left"),
            said("gone", "signed out", "signed out, choosing it signs it in"),
            said("gone-quiet", "signed out", "signed out"),
            said(
                "away",
                "not signed in",
                "not signed in, choosing it signs it in"
            ),
            said("away-quiet", "not signed in", "not signed in"),
            said(
                "away-unread",
                "not signed in",
                "not signed in, choosing it signs it in"
            ),
            said("unread", "not read", "not read"),
        ]
    );
    for auto_login in [true, false] {
        for (name, week, spoken) in words(auto_login) {
            let label = name
                .split_once('@')
                .map_or(name.as_str(), |(label, _)| label);
            let what = format!("{label} (auto-login {auto_login}): {week:?}, {spoken:?}");
            assert!(
                !week.contains("used") && !spoken.contains("used"),
                "the week is said left, never used: {what}"
            );
            assert!(
                week.match_indices('%')
                    .all(|(at, _)| week[at..].starts_with("% left")),
                "every figure says left: {what}"
            );
            assert!(
                !spoken.contains(['%', '\u{2264}', '\u{2265}']),
                "a reader hears the figure in words: {what}"
            );
            assert_eq!(
                week.starts_with("0% left"),
                label == "spent",
                "nothing left only where a reading said 100: {what}"
            );
            if label == "walled" {
                assert!(
                    !week.contains(|c: char| c.is_ascii_digit()),
                    "a wall says out, never a percentage: {what}"
                );
            }
        }
    }
}

#[test]
fn choose_fact_act_network_state_opens_lens_on_target_window() {
    let mut app = crate::App::headless_for_test();
    let w0 = crate::WindowId(0);
    let w1 = crate::WindowId(1);
    app.install_window_state(w1, crate::stub_session(99), 24, 80);
    app.frontmost_window = Some(w1);

    app.choose_fact_act(w0, 0, super::FactAct::NetworkState);
    assert_eq!(
        app.windows[&w0].stats_lens.open.map(|l| l.focus),
        Some(crate::stats_paint::Instrument::NetworkState),
        "NetworkState fact action on w0 must open the lens on w0 even when w1 is frontmost"
    );
    assert!(
        app.windows[&w1].stats_lens.open.is_none(),
        "w1 must not receive w0's NetworkState lens"
    );
}
