// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Account card's rows, state by state: what it says first, what each state's rows are,
//! and the two rules every row holds (none wider than its room, none cut).

use std::sync::Arc;

use aterm_agent::harness::account_card::{Budget, History, Machine, NoTable};
use aterm_agent::harness::account_progress::OwnClaimFinding;
use aterm_agent::harness::accounts;
use aterm_agent::harness::profile::{WeeklyFigure, WeeklyGap};
use aterm_agent::harness::token_state::Unverified;

use super::super::AccountLabel;
use super::*;
use crate::autopilot_menu::row_width;
use crate::token_shown::{TokenFact, TokenOffer};

/// Wed 11 Nov 2026, 18:20 UTC; the fixtures' local clock is UTC unless a test says otherwise.
const NOW: i64 = 1_794_421_200;
const HOUR: i64 = 3600;
const SAT_10: i64 = 1_794_650_400;
const TODAY_20_10: i64 = 1_794_427_800;
const FEB_1_2027: i64 = 1_801_483_200;
const OCT_2: i64 = 1_790_942_400;
const OCT_12: i64 = 1_791_806_400;
const OCT_28: i64 = 1_793_188_800;
const SEP_18: i64 = 1_789_732_800;
const NOV_30: i64 = 1_796_040_000;
const TODAY_13_05: i64 = 1_794_402_300;
const TODAY_09_40: i64 = 1_794_390_000;
const TODAY_21_40: i64 = 1_794_433_200;
const MON_18_40: i64 = 1_794_249_600;
const NOV_9: i64 = 1_794_182_400;
const NEXT_MON_09: i64 = 1_794_819_600;
const FRI_08: i64 = 1_794_556_800;
const MIDNIGHT: i64 = 1_794_355_200;

fn mac(name: &str, ours: bool) -> Mac {
    Mac {
        name: name.into(),
        ours,
        named: !name.is_empty(),
    }
}

fn card(who: Who) -> AccountCard {
    AccountCard {
        who,
        names: Default::default(),
        pool_size: 3,
        this_mac: Some("m20".into()),
        login: None,
        budget: Budget::default(),
        history: Arc::new(History::none(HistoryFrom::Nothing)),
        machine: Machine {
            home: Home::NoPoolFile,
            claims: Claims::Absent,
            last_use: None,
            used_day: None,
            starts: None,
            journal: JournalHeld::Off,
        },
        pinned: None,
        offsets: Offsets::fixed(0),
        read_at: NOW,
    }
}

fn acct(label: &str, name: &str, trail: &str, active: bool) -> AccountMenuRow {
    AccountMenuRow {
        account: AccountLabel::new(label).expect("a roster label"),
        name: name.into(),
        trail: trail.into(),
        spoken: format!("{name}, {trail}"),
        active,
        enabled: true,
    }
}

fn claude(tab: u8, email: &str, room: usize) -> FactMenuFacts {
    FactMenuFacts {
        title: format!("Account \u{b7} tab {tab}"),
        spoken: format!("Account, tab {tab}"),
        room,
        cli: Some(Cli::Claude),
        product: "Claude Code",
        email: Some(email.into()),
        now: NOW,
        movable: true,
        tabs_here: Some(1),
        ..FactMenuFacts::default()
    }
}

fn week(used_x10: u32, resets_at: i64) -> WeeklyFigure {
    WeeklyFigure {
        used_x10,
        resets_at,
        at: NOW,
    }
}

fn holder(mac: Mac, since: i64) -> Holder {
    Holder {
        mac,
        since: Some(since),
        until: TODAY_21_40,
    }
}

/// Readings every four hours over the last five days: the week runs down, is spent, a new
/// week begins, and three cells go unread before the last one.
fn journal_reads() -> Vec<Read> {
    let mut reads = Vec::new();
    for step in 0..30_i64 {
        let t = NOW - (29 - step) * 4 * HOUR;
        let (x10, reset) = match step {
            0..=19 => (
                u16::try_from(500 + step * 25).unwrap_or(0),
                SAT_10 - 7 * DAY,
            ),
            20 => (1000, SAT_10 - 7 * DAY),
            24..=26 => continue,
            _ => (u16::try_from((step - 21) * 20).unwrap_or(0), SAT_10),
        };
        reads.push(Read {
            t,
            x10,
            reset,
            epoch: None,
        });
    }
    reads
}

fn series_days(days: i64) -> Vec<DayPoint> {
    (0..days)
        .map(|back| {
            let left = u16::try_from(1000 - (back % 7) * 120).unwrap_or(0);
            DayPoint {
                midnight: MIDNIGHT - back * DAY,
                left_x10: match back {
                    15 => None,
                    17 | 32 => Some(0),
                    _ => Some(left),
                },
                out: back == 17,
                five_walled: matches!(back, 3 | 9 | 20),
            }
        })
        .rev()
        .collect()
}

fn pool_a() -> Vec<AccountMenuRow> {
    vec![
        acct("alpha", "alpha.m20.1@proton.me", "\u{2264}88% left", false),
        acct("beta", "beta.m21.1@proton.me", "not signed in", false),
        acct("gamma", "gamma.m22.1@proton.me", "not signed in", false),
    ]
}

/// (A): a login outside this Mac's pool, on a Mac that reads the lab's journal and claims.
fn state_a(room: usize) -> FactMenuFacts {
    let email = "unicorn.m15.2@proton.me";
    let m15 = mac("m15", false);
    let mut card = card(Who::Outside {
        email: email.into(),
    });
    card.history = Arc::new(History {
        from: HistoryFrom::Journal,
        reads: journal_reads(),
        days: Vec::new(),
        since: None,
        stamp: 1,
    });
    card.machine = Machine {
        home: Home::Known {
            mac: m15.clone(),
            why: Some(SlotWhy::Pin),
            since: Some(OCT_2),
            also: Vec::new(),
        },
        claims: Claims::Read(vec![holder(m15.clone(), TODAY_13_05)]),
        last_use: None,
        used_day: None,
        starts: Some((vec![(m15, 41), (mac("m20", true), 5)], OCT_28)),
        journal: JournalHeld::Held,
    };
    let mut token = TokenFact::for_test(None, email, TokenState::None);
    token.offer = Some(TokenOffer {
        line: "a vault token would keep work signed in: make one".into(),
        label: email.into(),
        replace: false,
    });
    FactMenuFacts {
        tokens_read: Some(TokensRead::Read),
        token: Some(token),
        own: Some(Figure::Exact(160)),
        own_reset: Some(SAT_10),
        own_at: Some(NOW),
        plan: Some("Ultra".into()),
        five_hour: Some(week(30, TODAY_20_10)),
        credits: Some("off"),
        route: Some(AuthRoute::Stored),
        card: Some(card),
        accounts: pool_a(),
        account_progress: Some(AccountProgress::unknown(NOW)),
        ..claude(2, email, room)
    }
}

/// (A, debug build): no journal, no claims table, and the lab does not key the account.
fn state_a_debug(room: usize) -> FactMenuFacts {
    let mut facts = state_a(room);
    facts.token = None;
    let card = facts.card.as_mut().expect("a card");
    card.history = Arc::new(History::none(HistoryFrom::Nothing));
    card.machine.claims = Claims::Absent;
    card.machine.starts = None;
    card.machine.journal = JournalHeld::Off;
    facts
}

/// (B): a pool account with a vault token, at home on this Mac and in use here.
fn state_b(room: usize) -> FactMenuFacts {
    let email = "sol.m20.1@proton.me";
    let here = mac("m20", true);
    let mut card = card(Who::Pool {
        label: "sol-1".into(),
        name: email.into(),
        enrolled: true,
    });
    card.pool_size = 2;
    card.login = Some(LoginHalf::InUse);
    card.budget = Budget {
        models: vec![(
            "Fable".into(),
            Seen {
                figure: Figure::Out,
                reset: Some(NEXT_MON_09),
                at: Some(NOW),
                by: None,
            },
        )],
        resets: Some(accounts::Offer {
            kind: None,
            left: 2,
            use_by: Some(NOV_30),
            at: NOW,
            by: String::new(),
        }),
        ..Budget::default()
    };
    card.history = Arc::new(History {
        from: HistoryFrom::Series,
        reads: journal_reads(),
        days: series_days(49),
        since: Some(SEP_18),
        stamp: 1,
    });
    card.machine = Machine {
        home: Home::Known {
            mac: here.clone(),
            why: Some(SlotWhy::Fill),
            since: Some(OCT_12),
            also: Vec::new(),
        },
        claims: Claims::Read(vec![holder(here.clone(), TODAY_09_40)]),
        last_use: None,
        used_day: None,
        starts: Some((vec![(here, 40), (mac("m22", false), 1)], OCT_28)),
        journal: JournalHeld::Held,
    };
    let mut token = TokenFact::for_test(Some("sol-1"), email, TokenState::Active);
    token.ends_at = u64::try_from(FEB_1_2027).ok();
    let mut progress = AccountProgress::unknown(NOW);
    progress.phase = ProgressPhase::Watching;
    (progress.eligible, progress.known) = (Some(1), Some(2));
    FactMenuFacts {
        tokens_read: Some(TokensRead::Read),
        token: Some(token),
        own: Some(Figure::Exact(390)),
        own_reset: Some(SAT_10),
        own_at: Some(NOW),
        plan: Some("Max 20x".into()),
        five_hour: Some(week(600, TODAY_20_10)),
        credits: Some("on"),
        route: accounts::TokenEntry::parse("aterm-claude-token-ac-fedcba9876543210")
            .map(|entry| AuthRoute::VaultToken { entry }),
        account_row: Some("sol-1".into()),
        card: Some(card),
        tabs_here: Some(3),
        accounts: vec![
            acct("sol-1", email, "61% left", true),
            acct("sol-2", "sol.m20.2@proton.me", "\u{2264}88% left", false),
        ],
        account_progress: Some(progress),
        ..claude(1, email, room)
    }
}

/// (C): a pool account whose home is another Mac; `held` is whether that Mac holds it now.
fn state_c(room: usize, held: bool) -> FactMenuFacts {
    let mut facts = state_b(room);
    let m22 = mac("m22", false);
    let here = mac("m20", true);
    let card = facts.card.as_mut().expect("a card");
    card.machine.home = Home::Known {
        mac: m22.clone(),
        why: Some(SlotWhy::Pin),
        since: Some(OCT_28),
        also: vec![here.clone()],
    };
    card.machine.starts = Some((vec![(m22.clone(), 28), (here.clone(), 2)], OCT_28));
    facts.tabs_here = Some(1);
    if held {
        card.machine.claims =
            Claims::Read(vec![holder(m22, TODAY_09_40), holder(here, TODAY_13_05)]);
        facts.account_progress.as_mut().expect("progress").own_claim = Some(OwnClaimFinding {
            blocker: OwnClaimBlocker::HeldElsewhere { until: TODAY_21_40 },
            pinned: false,
        });
    } else {
        card.machine.claims = Claims::Read(vec![holder(here, TODAY_13_05)]);
        card.machine.last_use = Some((m22, MON_18_40));
    }
    facts
}

/// (D1): the first paint, before the account reader and the token read answered.
fn state_d1(room: usize) -> FactMenuFacts {
    FactMenuFacts {
        own: Some(Figure::Exact(390)),
        own_reset: Some(SAT_10),
        own_at: Some(NOW),
        plan: Some("Max 20x".into()),
        five_hour: Some(week(600, TODAY_20_10)),
        accounts_loading: true,
        ..claude(3, "sol.m20.1@proton.me", room)
    }
}

/// (D5): the pool's rows could not be read; the card of the account still was.
fn state_d5(room: usize) -> FactMenuFacts {
    let mut facts = state_b(room);
    facts.accounts.clear();
    facts.accounts_refused = Some("accounts-state.json: unexpected end of file".into());
    facts
}

/// (F1): a pooled CLI that is not Claude Code, whose footer names no login.
fn state_f1(room: usize) -> FactMenuFacts {
    let here = mac("m20", true);
    let mut card = card(Who::Active {
        label: "dev".into(),
        name: "dev@example.com".into(),
    });
    card.pool_size = 2;
    card.login = Some(LoginHalf::Ok);
    card.budget.week = Some(Seen {
        figure: Figure::AtLeast(300),
        reset: Some(FRI_08),
        at: None,
        by: None,
    });
    card.history = Arc::new(History::none(HistoryFrom::NotKept));
    card.machine.home = Home::Known {
        mac: here.clone(),
        why: Some(SlotWhy::Fill),
        since: Some(OCT_12),
        also: Vec::new(),
    };
    card.machine.claims = Claims::Read(vec![holder(here, TODAY_09_40)]);
    FactMenuFacts {
        title: "Account \u{b7} tab 4".into(),
        spoken: "Account, tab 4".into(),
        room,
        cli: Some(Cli::Codex),
        product: "Codex",
        now: NOW,
        tabs_here: Some(1),
        card: Some(card),
        accounts: vec![
            acct("dev", "dev@example.com", "\u{2264}70% left", true),
            acct("ops", "ops@example.com", "signed out", false),
        ],
        ..FactMenuFacts::default()
    }
}

/// (F2): a program that keeps its own login; no account reader runs for it.
fn state_f2(room: usize) -> FactMenuFacts {
    FactMenuFacts {
        title: "Account \u{b7} tab 5".into(),
        spoken: "Account, tab 5".into(),
        room,
        product: "Muse",
        email: Some("me@example.com".into()),
        now: NOW,
        tabs_here: Some(1),
        own: Some(Figure::Exact(450)),
        own_reset: Some(NEXT_MON_09),
        plan: Some("Pro".into()),
        refresh_usage: true,
        ..FactMenuFacts::default()
    }
}

/// Every state the card has, by name, at `room`.
fn states(room: usize) -> Vec<(&'static str, FactMenuFacts)> {
    let mut d3 = state_a_debug(room);
    d3.tokens_read = Some(TokensRead::NoVault);
    let mut d4 = state_b(room);
    d4.card.as_mut().expect("a card").machine.home = Home::NoPoolFile;
    let mut e_refused = state_b(room);
    e_refused.forecast_refusal = Some(outlook::Refusal::SessionPaused);
    let mut e_outside = state_b(room);
    e_outside.movable = false;
    let mut kimi = state_f1(room);
    (kimi.cli, kimi.product) = (Some(Cli::Kimi), "Kimi");
    kimi.card.as_mut().expect("a card").machine.claims = Claims::NotTracked;
    let mut empty = state_f1(room);
    empty.accounts.clear();
    let mut agy = state_f2(room);
    (agy.product, agy.refresh_usage, agy.signed_out) = ("Antigravity", false, true);
    let mut long = state_b(room);
    long.title = "Account \u{b7} a folder with a long name, tab 12".into();
    long.name = Some("a session named at some length by its person".into());
    long.note = Some("Claude Code is busy: the account was not changed".into());
    long.pinned = Some("sol.m20.1@proton.me".into());
    vec![
        ("A", state_a(room)),
        ("A debug", state_a_debug(room)),
        ("B", state_b(room)),
        ("C held", state_c(room, true)),
        ("C not held", state_c(room, false)),
        ("D1", state_d1(room)),
        ("D3", d3),
        ("D4", d4),
        ("D5", state_d5(room)),
        ("E refused", e_refused),
        ("E outside", e_outside),
        ("F1", state_f1(room)),
        ("F1 Kimi", kimi),
        ("F1 empty", empty),
        ("F2", state_f2(room)),
        ("F2 signed out", agy),
        ("long names", long),
    ]
}

/// The rows as a person reads them: a fact's name in its column, an item's trail in brackets
/// and its check as `●`, a heading with a stub of its rule, a chart as `<chart>` (its cells
/// have their own tests) and the legend as its words.
fn lines(rows: &[MenuRow]) -> Vec<String> {
    rows.iter()
        .map(|row| match row {
            MenuRow::Header(text) | MenuRow::Note { text, .. } => text.clone(),
            MenuRow::Name(name) => format!("\u{201c}{name}\u{201d}"),
            MenuRow::Section(title) => format!("{title} \u{2500}\u{2500}"),
            MenuRow::Fact { label, value, .. } => format!("{label:<9}{value}"),
            MenuRow::Item {
                glyph,
                label,
                trail,
                checked,
                ..
            } => {
                let mut line = format!("{glyph}  {label}");
                if !trail.is_empty() {
                    line.push_str(&format!("  [{trail}]"));
                }
                if *checked {
                    line.push_str(" \u{25cf}");
                }
                line
            }
            MenuRow::Plot { cells, .. } => {
                let text: String = cells.iter().map(|cell| cell.ch).collect();
                if text.contains("week reset") {
                    text
                } else {
                    "<chart>".into()
                }
            }
            other => format!("{other:?}"),
        })
        .collect()
}

fn shown(facts: &FactMenuFacts) -> Vec<String> {
    lines(&account_rows(facts))
}

/// Every word a row shows or says.
fn words_of(rows: &[MenuRow]) -> Vec<String> {
    let mut out = Vec::new();
    for row in rows {
        match row {
            MenuRow::Header(text) | MenuRow::Section(text) | MenuRow::Name(text) => {
                out.push(text.clone());
            }
            MenuRow::Note { text, spoken, .. } => out.extend([text.clone(), spoken.clone()]),
            MenuRow::Fact {
                label,
                value,
                spoken,
                ..
            } => out.extend([label.clone(), value.clone(), spoken.clone()]),
            MenuRow::Item {
                label,
                spoken,
                trail,
                ..
            } => out.extend([label.clone(), spoken.clone(), trail.clone()]),
            MenuRow::Plot { cells, spoken, .. } => {
                out.push(cells.iter().map(|cell| cell.ch).collect());
                out.push(spoken.clone());
            }
            MenuRow::Rule | MenuRow::Gap(_) | MenuRow::Stops { .. } => {}
        }
    }
    out
}

fn fact_value(rows: &[MenuRow], name: &str) -> Option<String> {
    let at = rows
        .iter()
        .position(|row| matches!(row, MenuRow::Fact { label, .. } if label == name))?;
    let mut parts = Vec::new();
    for row in &rows[at..] {
        match row {
            MenuRow::Fact { label, value, .. } if label == name || label.is_empty() => {
                parts.push(value.clone());
            }
            _ => break,
        }
    }
    Some(parts.join(" "))
}

/// The value of the `nth` fact named `name` (the verdict is the first, its section's the next).
fn nth_fact(rows: &[MenuRow], name: &str, nth: usize) -> Option<(String, NoteTone)> {
    rows.iter()
        .filter_map(|row| match row {
            MenuRow::Fact {
                label, value, tone, ..
            } if label == name => Some((value.clone(), *tone)),
            _ => None,
        })
        .nth(nth)
}

/// THE CARD ANSWERS TOKEN, BUDGET, MACHINE FIRST. In every state, at every width, the three
/// answers directly follow the head (the card's name, the account's address, a notice), in that
/// order, before any heading.
#[test]
fn the_account_card_answers_token_budget_machine_first() {
    for room in [24, 44, 60] {
        for (name, facts) in states(room) {
            let rows = account_rows(&facts);
            let first = rows
                .iter()
                .position(|row| {
                    !matches!(
                        row,
                        MenuRow::Header(_) | MenuRow::Name(_) | MenuRow::Note { .. }
                    )
                })
                .unwrap_or_else(|| panic!("{name} at {room}: no answer"));
            let answers: Vec<&str> = rows[first..]
                .iter()
                .take_while(|row| !matches!(row, MenuRow::Section(_)))
                .filter_map(|row| match row {
                    MenuRow::Fact { label, .. } if !label.is_empty() => Some(label.as_str()),
                    _ => None,
                })
                .take(3)
                .collect();
            assert_eq!(answers, ["Token", "Budget", "Machine"], "{name} at {room}");
            assert!(
                matches!(&rows[first], MenuRow::Fact { label, .. } if label == "Token"),
                "{name} at {room}: {:?}",
                rows[first]
            );
        }
    }
}

#[test]
fn state_a_a_login_outside_this_macs_pool() {
    assert_eq!(
        shown(&state_a(60)),
        [
            "Account \u{b7} tab 2",
            "\u{2386}  unicorn.m15.2@proton.me",
            "Token    none in the vault",
            "Budget   84% left \u{b7} resets Sat 10:00",
            "Machine  home m15 \u{b7} in use there",
            "Token \u{2500}\u{2500}",
            "Login    signed in on this Mac \u{b7} this tab runs on it",
            "+  Make a vault token",
            "Budget \u{2500}\u{2500}",
            "Plan     Ultra",
            "5-hour   97% left \u{b7} resets 20:10",
            "Credits  off",
            "History  week left, as the lab's Macs read it",
            "<chart>",
            "<chart>",
            "<chart>",
            "         \u{21ba} week reset  \u{2573} out  \u{2504} not read",
            "Machine \u{2500}\u{2500}",
            "Home     m15 \u{b7} set on the accounts sheet since 2 Oct",
            "m15      in use \u{b7} claimed since 13:05",
            "This Mac in use: this tab only",
            "Usual    m15 \u{b7} 41 of 46 starts since 28 Oct",
            "Switch this Mac to \u{2500}\u{2500}",
            "\u{2386}  alpha.m20.1@proton.me  [\u{2264}88% left]",
            "\u{2386}  beta.m21.1@proton.me  [not signed in]",
            "\u{2386}  gamma.m22.1@proton.me  [not signed in]",
            "Automatic switching \u{2500}\u{2500}",
            "Off for this tab: its login is not one of this Mac's 3",
            "accounts.",
            "Pin      none: aterm picks the account",
            "\u{21c4}  Move this Mac to the next account",
            "\u{29c9}  Copy details",
            "\u{22ef}  Network State\u{2026}",
            "\u{22ef}  Account settings\u{2026}",
        ]
    );
}

#[test]
fn state_a_on_a_debug_build_says_what_it_does_not_know() {
    assert_eq!(
        shown(&state_a_debug(44)),
        [
            "Account \u{b7} tab 2",
            "\u{2386}  unicorn.m15.2@proton.me",
            "Token    not checked on this Mac",
            "Budget   84% left \u{b7} resets Sat 10:00",
            "Machine  home m15 \u{b7} use there not known",
            "Token \u{2500}\u{2500}",
            "Login    signed in on this Mac",
            "         this tab runs on it",
            "Budget \u{2500}\u{2500}",
            "Plan     Ultra",
            "5-hour   97% left \u{b7} resets 20:10",
            "Credits  off",
            "History  none kept on this Mac for this",
            "         account",
            "Machine \u{2500}\u{2500}",
            "Home     m15 \u{b7} set on the accounts sheet",
            "         since 2 Oct",
            "m15      not known: this Mac does not read",
            "         other Macs",
            "This Mac in use: this tab only",
            "Switch this Mac to \u{2500}\u{2500}",
            "\u{2386}  alpha.m20.1@proton.me  [\u{2264}88% left]",
            "\u{2386}  beta.m21.1@proton.me  [not signed in]",
            "\u{2386}  gamma.m22.1@proton.me  [not signed in]",
            "Automatic switching \u{2500}\u{2500}",
            "Off for this tab: its login is not one of",
            "this Mac's 3 accounts.",
            "Pin      none: aterm picks the account",
            "\u{21c4}  Move this Mac to the next account",
            "\u{29c9}  Copy details",
            "\u{22ef}  Network State\u{2026}",
            "\u{22ef}  Account settings\u{2026}",
        ]
    );
}

#[test]
fn state_b_a_pool_account_at_home_here_with_a_vault_token() {
    assert_eq!(
        shown(&state_b(60)),
        [
            "Account \u{b7} tab 1",
            "\u{2386}  sol.m20.1@proton.me",
            "Token    in the vault \u{b7} good until 1 Feb 2027",
            "Budget   61% left \u{b7} resets Sat 10:00",
            "Machine  home this Mac \u{b7} in use here",
            "Token \u{2500}\u{2500}",
            "Login    not needed: this tab runs on the vault token",
            "Budget \u{2500}\u{2500}",
            "Plan     Max 20x",
            "5-hour   40% left \u{b7} resets 20:10",
            "Fable    week out until Mon 09:00",
            "Credits  on",
            "Resets   2 limit resets left \u{b7} use by 30 Nov",
            "Forecast not worked out yet",
            "History  week left, read on this Mac since 18 Sep",
            "<chart>",
            "<chart>",
            "<chart>",
            "<chart>",
            "<chart>",
            "         \u{21ba} week reset  \u{2573} out  \u{2504} not read",
            "5-hour limit reached on 3 of the 49 days read",
            "Machine \u{2500}\u{2500}",
            "Home     this Mac (m20) \u{b7} assigned since 12 Oct",
            "This Mac in use: this tab and 2 more",
            "Others   no other Mac is using it",
            "Usual    this Mac \u{b7} 40 of 41 starts since 28 Oct",
            "Switch this Mac to \u{2500}\u{2500}",
            "\u{2386}  sol.m20.1@proton.me  [61% left] \u{25cf}",
            "\u{2386}  sol.m20.2@proton.me  [\u{2264}88% left]",
            "Automatic switching \u{2500}\u{2500}",
            "On: 1 of this Mac's 2 accounts can take over.",
            "Pin      none: aterm picks the account",
            "\u{21c4}  Move this Mac to the next account",
            "\u{29c9}  Copy details",
            "\u{22ef}  Network State\u{2026}",
            "\u{22ef}  Account settings\u{2026}",
        ]
    );
    // What is not held is not a row, and no offer stands without one.
    let mut bare = state_b(60);
    let card = bare.card.as_mut().expect("a card");
    card.budget = Budget::default();
    let rows = shown(&bare);
    for gone in ["Fable", "Resets", "+  "] {
        assert!(
            !rows.iter().any(|row| row.starts_with(gone)),
            "{gone}: {rows:?}"
        );
    }
}

#[test]
fn state_c_a_home_on_another_mac() {
    let held = shown(&state_c(60, true));
    assert_eq!(held[4], "Machine  home m22 \u{b7} in use there and here");
    let at = held
        .iter()
        .position(|row| row == "Machine \u{2500}\u{2500}")
        .expect("the Machine section");
    assert_eq!(
        held[at + 1..at + 6],
        [
            "Home     m22 \u{b7} set on the accounts sheet since 28 Oct",
            "         also assigned to this Mac",
            "m22      in use \u{b7} claimed since 09:40",
            "This Mac in use: this tab only",
            "Usual    m22 \u{b7} 28 of 30 starts since 28 Oct",
        ]
    );
    assert!(
        held.contains(&"m22 holds this account until 21:40.".to_owned()),
        "{held:?}"
    );

    let mut facts = state_c(60, false);
    let rows = shown(&facts);
    assert_eq!(rows[4], "Machine  home m22 \u{b7} not in use there");
    assert!(
        rows.contains(&"m22      not in use \u{b7} last used Mon 9 Nov 18:40".to_owned()),
        "the journal's last use: {rows:?}"
    );
    let machine = &mut facts.card.as_mut().expect("a card").machine;
    (machine.last_use, machine.used_day) = (None, Some(NOV_9));
    let rows = shown(&facts);
    assert!(
        rows.contains(&"m22      not in use \u{b7} last used 9 Nov".to_owned()),
        "the pool file's day: {rows:?}"
    );
    facts.card.as_mut().expect("a card").machine.used_day = None;
    let rows = shown(&facts);
    assert!(rows.contains(&"m22      not in use".to_owned()), "{rows:?}");
}

#[test]
fn state_d_what_is_not_read_yet_is_said_so() {
    assert_eq!(
        shown(&state_d1(44)),
        [
            "Account \u{b7} tab 3",
            "\u{2386}  sol.m20.1@proton.me",
            "Token    not read yet",
            "Budget   61% left \u{b7} resets Sat 10:00",
            "Machine  not read yet",
            "Budget \u{2500}\u{2500}",
            "Plan     Max 20x",
            "5-hour   40% left \u{b7} resets 20:10",
            "Reading this Mac's accounts",
            "\u{22ef}  Account settings\u{2026}",
        ]
    );

    // D2: no journal. `Usual` is dropped and the Mac rows come from the claims alone.
    let mut d2 = state_c(60, true);
    let machine = &mut d2.card.as_mut().expect("a card").machine;
    (machine.journal, machine.starts) = (JournalHeld::Off, None);
    let rows = shown(&d2);
    assert!(!rows.iter().any(|row| row.starts_with("Usual")), "{rows:?}");
    assert!(rows.contains(&"m22      in use \u{b7} claimed since 09:40".to_owned()));

    // D3: no vault is not "no token"; a vault that could not be read is not known.
    let mut d3 = state_a_debug(44);
    d3.tokens_read = Some(TokensRead::NoVault);
    assert_eq!(shown(&d3)[2], "Token    no vault on this Mac");
    d3.tokens_read = Some(TokensRead::Read);
    d3.token = Some(TokenFact::for_test(
        None,
        "unicorn.m15.2@proton.me",
        TokenState::Unknown,
    ));
    let rows = shown(&d3);
    assert_eq!(
        rows[2..4],
        [
            "Token    not known: the vault could not be",
            "         read"
        ]
    );

    // D4: no pool file, and an address the file does not list.
    let mut d4 = state_b(60);
    d4.card.as_mut().expect("a card").machine.home = Home::NoPoolFile;
    let rows = shown(&d4);
    assert_eq!(rows[4], "Machine  home not known \u{b7} in use here");
    assert!(rows.contains(&"Home     not known: no pool list on this Mac".to_owned()));
    d4.card.as_mut().expect("a card").machine.home = Home::NotListed;
    let rows = shown(&d4);
    assert_eq!(rows[4], "Machine  no home on record \u{b7} in use here");
    assert!(rows.contains(&"Home     no home on record".to_owned()));

    // D5: the pool's rows failed; the token, the budget and the machine stay.
    let rows = shown(&state_d5(60));
    assert_eq!(
        rows[2],
        "Token    in the vault \u{b7} good until 1 Feb 2027"
    );
    assert_eq!(rows[4], "Machine  home this Mac \u{b7} in use here");
    assert!(rows.contains(&"Home     this Mac (m20) \u{b7} assigned since 12 Oct".to_owned()));
    let at = rows
        .iter()
        .position(|row| row.starts_with("This Mac's accounts could not be read:"))
        .expect("the refusal");
    assert_eq!(
        rows[at..],
        [
            "This Mac's accounts could not be read: accounts-state.json:",
            "unexpected end of file",
            "\u{22ef}  Account settings\u{2026}",
        ]
    );
    assert!(!rows.iter().any(|row| row.starts_with("Switch this Mac")));
}

/// THE FORECAST ROW FOLLOWS ITS PERMISSION (E). First that applies: an account that is no
/// Claude Code pool row has no row; a recovery that refuses says its refusal; a tab the
/// rotation cannot move says that; with no outlook, or no projection of this row, it is not
/// worked out yet.
#[test]
fn the_forecast_row_follows_its_permission() {
    let forecast = |facts: &FactMenuFacts| fact_value(&account_rows(facts), "Forecast");
    let mut facts = state_b(60);
    assert_eq!(forecast(&facts).as_deref(), Some("not worked out yet"));
    facts.movable = false;
    assert_eq!(
        forecast(&facts).as_deref(),
        Some("none: aterm cannot switch this tab")
    );
    facts.forecast_refusal = Some(outlook::Refusal::SessionPaused);
    assert_eq!(
        forecast(&facts).as_deref(),
        Some("none: session recovery paused"),
        "a refusal is said before the route"
    );
    assert_eq!(forecast(&state_a(60)), None, "an outside account");
    assert_eq!(forecast(&state_f1(60)), None, "another program's pool");
    assert_eq!(forecast(&state_d1(60)), None, "not read yet");
}

#[test]
fn state_f_a_program_that_is_not_claude_code() {
    assert_eq!(
        shown(&state_f1(44)),
        [
            "Account \u{b7} tab 4",
            "\u{2386}  dev@example.com",
            "   Codex's current account on this Mac",
            "Token    not used: Codex signs in by itself",
            "Budget   \u{2264}70% left \u{b7} resets Fri 08:00",
            "Machine  home this Mac \u{b7} in use here",
            "Token \u{2500}\u{2500}",
            "Login    signed in on this Mac",
            "Budget \u{2500}\u{2500}",
            "History  not kept for Codex accounts",
            "Machine \u{2500}\u{2500}",
            "Home     this Mac (m20)",
            "         assigned since 12 Oct",
            "This Mac in use: this tab only",
            "Others   no other Mac is using it",
            "Switch this Mac to \u{2500}\u{2500}",
            "\u{2386}  dev@example.com  [\u{2264}70% left] \u{25cf}",
            "\u{2386}  ops@example.com  [signed out]",
            "Automatic switching \u{2500}\u{2500}",
            "Pin      none: aterm picks the account",
            "\u{21c4}  Move this Mac to the next account",
            "\u{22ef}  Account settings\u{2026}",
        ]
    );

    let mut kimi = state_f1(44);
    (kimi.cli, kimi.product) = (Some(Cli::Kimi), "Kimi");
    kimi.card.as_mut().expect("a card").machine.claims = Claims::NotTracked;
    let rows = shown(&kimi);
    assert!(
        rows.contains(&"Others   not tracked for Kimi accounts".to_owned()),
        "{rows:?}"
    );

    let mut empty = state_f1(44);
    empty.accounts.clear();
    let rows = shown(&empty);
    let at = rows
        .iter()
        .position(|row| row.starts_with("No Codex accounts"))
        .expect("the empty roster");
    assert_eq!(
        rows[at..],
        [
            "No Codex accounts are listed on this Mac.",
            "Add one: aterm harness accounts add --cli",
            "codex <label>",
            "\u{22ef}  Account settings\u{2026}",
        ]
    );
    let mut none = state_b(44);
    none.accounts.clear();
    let rows = account_rows(&none);
    assert!(
        rows.iter().any(|row| matches!(
            row,
            MenuRow::Item { act: MenuAct::Fact(Fact::Account, FactAct::AutoRotate), label, .. }
                if label.starts_with("Sync accounts")
        )),
        "Claude Code's empty roster keeps its sync item"
    );

    assert_eq!(
        shown(&state_f2(44)),
        [
            "Account \u{b7} tab 5",
            "\u{2386}  me@example.com",
            "Token    not used: Muse keeps its own login",
            "Budget   55% left \u{b7} resets Mon 09:00",
            "Machine  in use here",
            "         other Macs not tracked",
            "Budget \u{2500}\u{2500}",
            "Plan     Pro",
            "Muse keeps its own login: switch accounts",
            "with /login in Muse.",
            "\u{21c4}  Refresh usage",
        ]
    );
    let mut agy = state_f2(44);
    (agy.product, agy.refresh_usage, agy.signed_out) = ("Antigravity", false, true);
    assert_eq!(
        shown(&agy),
        [
            "Account \u{b7} tab 5",
            "\u{2386}  not signed in",
            "Token    not used: Antigravity keeps its own",
            "         login",
            "Budget   not known: not signed in",
            "Machine  in use here",
            "         other Macs not tracked",
            "Budget \u{2500}\u{2500}",
            "Plan     Pro",
            "Antigravity keeps its own login.",
        ]
    );
}

/// NO ROW IS WIDER THAN ITS ROOM: every state, at 20, 24, 33, 44 and 60 cells.
#[test]
fn no_account_card_row_is_wider_than_its_room() {
    let mut charts = 0;
    for room in [20, 24, 33, 44, 60] {
        for (name, facts) in states(room) {
            for row in account_rows(&facts) {
                charts += usize::from(matches!(row, MenuRow::Plot { .. }));
                assert!(
                    row_width(&row) <= room,
                    "{name} at {room}: {} cells: {row:?}",
                    row_width(&row)
                );
            }
        }
    }
    assert!(charts > 0, "the charts were measured too");
    // CONTROL: the measure sees a row that does not fit.
    let wide = MenuRow::Fact {
        label: "Login".into(),
        value: "signed in on this Mac \u{b7} this tab runs on it".into(),
        spoken: String::new(),
        tone: NoteTone::Value,
    };
    assert!(row_width(&wide) > 44);
}

/// EVERY WEEK FIGURE ON THE CARD SAYS WHAT IS LEFT: a `%` is followed by ` left`, or is the
/// pace's share of a week a day, so none is a share used.
#[test]
fn every_week_figure_on_the_card_says_left() {
    let mut figures = 0;
    for room in [20, 24, 33, 44, 60] {
        for (name, facts) in states(room) {
            for row in account_rows(&facts) {
                let (shown, spoken) = match &row {
                    MenuRow::Header(text) => (text.clone(), String::new()),
                    MenuRow::Note { text, spoken, .. } => (text.clone(), spoken.clone()),
                    MenuRow::Fact { value, spoken, .. } => (value.clone(), spoken.clone()),
                    MenuRow::Item { trail, spoken, .. } => (trail.clone(), spoken.clone()),
                    MenuRow::Plot { spoken, .. } => (String::new(), spoken.clone()),
                    _ => continue,
                };
                let what = format!("{name} at {room}: {row:?}");
                for (at, _) in shown.match_indices('%') {
                    figures += 1;
                    let rest = &shown[at..];
                    assert!(
                        rest.starts_with("% left") || rest.starts_with("% of a week a day"),
                        "{what}"
                    );
                }
                for (at, _) in spoken.match_indices(" percent") {
                    let rest = &spoken[at..];
                    assert!(
                        rest.starts_with(" percent left")
                            || rest.starts_with(" percent of a week a day"),
                        "{what}"
                    );
                }
            }
        }
    }
    assert!(figures > 0, "the states hold figures");
}

/// NO ROW IS CUT, from 24 cells up: the only `…` on the card end the two items that open
/// something. Nothing a row holds is shortened to fit: every word of each state's wide card is
/// on its narrow one.
#[test]
fn no_account_card_row_is_cut() {
    for room in [24, 33, 44, 60] {
        for (name, facts) in states(room) {
            for row in account_rows(&facts) {
                match &row {
                    MenuRow::Item { label, .. } if label.contains('\u{2026}') => {
                        assert!(
                            ["Network State\u{2026}", "Account settings\u{2026}"]
                                .contains(&label.as_str()),
                            "{name} at {room}: {label}"
                        );
                    }
                    other => {
                        let cut = match other {
                            MenuRow::Header(text) | MenuRow::Note { text, .. } => {
                                text.contains('\u{2026}')
                            }
                            MenuRow::Fact { value, .. } => value.contains('\u{2026}'),
                            MenuRow::Item { trail, .. } => trail.contains('\u{2026}'),
                            _ => false,
                        };
                        assert!(!cut, "{name} at {room}: {other:?}");
                    }
                }
            }
        }
    }
    // Wrapping drops nothing: where no chart stands in for words, the narrow card's letters
    // are the wide card's, in order.
    let letters = |facts: &FactMenuFacts| -> String {
        account_rows(facts)
            .iter()
            .flat_map(|row| match row {
                MenuRow::Header(text) | MenuRow::Note { text, .. } => vec![text.clone()],
                MenuRow::Fact { label, value, .. } => vec![label.clone(), value.clone()],
                MenuRow::Item { label, trail, .. } => vec![label.clone(), trail.clone()],
                _ => Vec::new(),
            })
            .collect::<String>()
            .replace([' ', '\u{b7}'], "")
    };
    let mut compared = 0;
    for ((name, wide), (_, narrow)) in states(44).iter().zip(states(24).iter()) {
        if [
            "A debug",
            "D1",
            "D3",
            "F1",
            "F1 Kimi",
            "F1 empty",
            "F2",
            "F2 signed out",
        ]
        .contains(name)
        {
            assert_eq!(letters(wide), letters(narrow), "{name}");
            compared += 1;
        }
    }
    assert_eq!(compared, 8);
}

/// EVERY TIME ON THE CARD IS ON THE LOCAL CLOCK: no row shows or says an RFC 3339 instant, and
/// a clock east of UTC moves the words with it.
#[test]
fn every_card_time_is_on_the_local_clock() {
    let stamped = |text: &str| {
        let bytes = text.as_bytes();
        bytes.windows(11).any(|w| {
            w[..4].iter().all(u8::is_ascii_digit)
                && w[4] == b'-'
                && w[5..7].iter().all(u8::is_ascii_digit)
                && w[7] == b'-'
                && w[8..10].iter().all(u8::is_ascii_digit)
                && w[10] == b'T'
        })
    };
    assert!(
        stamped("at 2026-11-11T18:20:00Z"),
        "CONTROL: the scan finds one"
    );
    for room in [24, 44, 60] {
        for (name, facts) in states(room) {
            for text in words_of(&account_rows(&facts)) {
                assert!(!stamped(&text), "{name} at {room}: {text}");
            }
        }
    }
    let mut east = state_b(60);
    east.card.as_mut().expect("a card").offsets = Offsets::fixed(9 * HOUR);
    let rows = shown(&east);
    assert_eq!(rows[3], "Budget   61% left \u{b7} resets Sat 19:00");
    assert!(
        rows.contains(&"5-hour   40% left \u{b7} resets 05:10".to_owned()),
        "{rows:?}"
    );
}

/// THE CARD NAMES NO ROSTER LABEL: an account is its address everywhere a person reads or
/// hears it.
#[test]
fn the_card_names_no_roster_label() {
    let mut facts = state_c(60, true);
    facts.accounts = vec![
        acct("zq-one", "sol.m20.1@proton.me", "61% left", true),
        acct("zq-two", "sol.m20.2@proton.me", "out", false),
    ];
    facts.account_row = Some("zq-one".into());
    facts.pinned = Some("sol.m20.1@proton.me".into());
    if let Some(Who::Pool { label, .. }) = facts.card.as_mut().map(|card| &mut card.who) {
        *label = "zq-one".into();
    }
    for text in words_of(&account_rows(&facts)) {
        assert!(!text.contains("zq-"), "{text}");
    }
    let rows = shown(&facts);
    assert!(rows.contains(&"Pin      this Mac stays on sol.m20.1@proton.me".to_owned()));
}

/// EVERY TOKEN STATE HAS ONE VERDICT, and each its tone.
#[test]
fn every_token_state_has_one_verdict() {
    let email = "sol.m20.1@proton.me";
    let verdict = |read: Option<TokensRead>, token: Option<TokenFact>| {
        let mut facts = state_b(120);
        (facts.tokens_read, facts.token) = (read, token);
        // The verdict is one fact however long: a 120-cell room keeps it on one row.
        let rows = super::build(&facts, 120, false);
        nth_fact(&rows, "Token", 0).expect("the Token verdict")
    };
    let of = |kind: TokenState, ends: Option<i64>, not_yet: bool| {
        let mut token = TokenFact::for_test(Some("sol-1"), email, kind);
        token.ends_at = ends.and_then(|at| u64::try_from(at).ok());
        token.not_yet = not_yet;
        verdict(Some(TokensRead::Read), Some(token))
    };
    let unverified = |minted_here: bool, minted_on: &str| TokenState::Unverified {
        minted_here,
        why: Unverified::NoReadingYet,
        minted_on: minted_on.into(),
        exposed: Vec::new(),
    };
    let cases: Vec<((String, NoteTone), &str, NoteTone)> = vec![
        (
            of(TokenState::Active, Some(FEB_1_2027), false),
            "in the vault \u{b7} good until 1 Feb 2027",
            NoteTone::Value,
        ),
        (
            of(TokenState::Soon, Some(NOV_30), false),
            "in the vault \u{b7} ends 30 Nov",
            NoteTone::Warn,
        ),
        (
            of(TokenState::None, None, false),
            "none in the vault",
            NoteTone::Value,
        ),
        (
            of(TokenState::None, None, true),
            "made here \u{b7} not in the vault yet",
            NoteTone::Value,
        ),
        (
            of(TokenState::Unknown, None, false),
            "not known: the vault could not be read",
            NoteTone::Label,
        ),
        (
            of(TokenState::Unreadable, None, false),
            "in the vault \u{b7} this Mac cannot open it",
            NoteTone::Warn,
        ),
        (
            of(
                TokenState::Mismatched(aterm_secrets::entry_class::Mismatch::Draft),
                None,
                false,
            ),
            "in the vault \u{b7} failed its check",
            NoteTone::Warn,
        ),
        (
            of(TokenState::Ended, Some(NOV_9), false),
            "in the vault \u{b7} ended 9 Nov",
            NoteTone::Warn,
        ),
        (
            of(TokenState::Rejected, None, false),
            "in the vault \u{b7} Claude Code refused it",
            NoteTone::Warn,
        ),
        (
            of(unverified(true, "m20"), None, false),
            "made here \u{b7} not verified yet",
            NoteTone::Value,
        ),
        (
            of(unverified(false, "m15"), None, false),
            "in the vault \u{b7} works only on m15 until verified",
            NoteTone::Value,
        ),
        (
            of(TokenState::Exposed(vec!["m9".into()]), None, false),
            "in the vault \u{b7} replace it: a removed Mac held it",
            NoteTone::Warn,
        ),
        (
            verdict(Some(TokensRead::Read), None),
            "not checked on this Mac",
            NoteTone::Label,
        ),
        (
            verdict(Some(TokensRead::NoVault), None),
            "no vault on this Mac",
            NoteTone::Label,
        ),
        (verdict(None, None), "not read yet", NoteTone::Label),
    ];
    let mut seen = std::collections::BTreeSet::new();
    for ((value, tone), want, want_tone) in cases {
        assert_eq!(value, want);
        assert_eq!(tone, want_tone, "{want}");
        assert!(seen.insert(value), "{want}: said by two states");
    }
    // A token signed only into this Mac's log so far is not said to be in the vault, whatever
    // state it is in: each state keeps its clause and its tone.
    let made = "made here \u{b7} not in the vault yet";
    let not_yet: Vec<((String, NoteTone), String, NoteTone)> = vec![
        (
            of(TokenState::Active, Some(FEB_1_2027), true),
            format!("{made} \u{b7} good until 1 Feb 2027"),
            NoteTone::Value,
        ),
        (
            of(TokenState::Soon, Some(NOV_30), true),
            format!("{made} \u{b7} ends 30 Nov"),
            NoteTone::Warn,
        ),
        (
            of(TokenState::Unreadable, None, true),
            format!("{made} \u{b7} this Mac cannot open it"),
            NoteTone::Warn,
        ),
        (
            of(
                TokenState::Mismatched(aterm_secrets::entry_class::Mismatch::Draft),
                None,
                true,
            ),
            format!("{made} \u{b7} failed its check"),
            NoteTone::Warn,
        ),
        (
            of(TokenState::Ended, Some(NOV_9), true),
            format!("{made} \u{b7} ended 9 Nov"),
            NoteTone::Warn,
        ),
        (
            of(TokenState::Rejected, None, true),
            format!("{made} \u{b7} Claude Code refused it"),
            NoteTone::Warn,
        ),
        (
            of(unverified(true, "m20"), None, true),
            format!("{made} \u{b7} not verified yet"),
            NoteTone::Value,
        ),
        (
            of(unverified(false, "m15"), None, true),
            format!("{made} \u{b7} works only on m15 until verified"),
            NoteTone::Value,
        ),
        (
            of(TokenState::Exposed(vec!["m9".into()]), None, true),
            format!("{made} \u{b7} replace it: a removed Mac held it"),
            NoteTone::Warn,
        ),
    ];
    for ((value, tone), want, want_tone) in not_yet {
        assert_eq!(value, want);
        assert_eq!(tone, want_tone, "{want}");
        assert!(!value.contains("in the vault \u{b7}"), "{value}");
        assert!(seen.insert(value), "{want}: said by two states");
    }
    let rows = account_rows(&state_f1(44));
    assert_eq!(
        nth_fact(&rows, "Token", 0),
        Some(("not used: Codex signs in by itself".into(), NoteTone::Label))
    );
}

/// A MISSING TOKEN FACT SAYS NOT CHECKED, NEVER NONE: the vault was read and holds no word of
/// this account (the lab does not key it), which is not the vault holding no token for it.
#[test]
fn a_missing_token_fact_says_not_checked_never_none() {
    let rows = account_rows(&state_a_debug(60));
    let (value, tone) = nth_fact(&rows, "Token", 0).expect("the verdict");
    assert_eq!(value, "not checked on this Mac");
    assert_eq!(tone, NoteTone::Label);
    assert!(
        !words_of(&rows)
            .iter()
            .any(|text| text.contains("none in the vault"))
    );
    // CONTROL: a fact that says none is said as none.
    let rows = account_rows(&state_a(60));
    assert_eq!(
        nth_fact(&rows, "Token", 0).expect("the verdict").0,
        "none in the vault"
    );
}

/// BEFORE THE CARD, THE LOGIN ROW SAYS ONLY WHAT THE TOKEN READ HOLDS: with no card read yet,
/// a row with no login on this Mac is said never to have signed in here, and an account whose
/// login nothing read gets no Login row, rather than either being called signed in. CONTROL:
/// a held login's row says it is signed in.
#[test]
fn before_the_card_the_login_row_follows_the_token_reads_login() {
    let login_row = |login: LoginHere| {
        let mut facts = state_a(60);
        facts.card = None;
        facts.token.as_mut().expect("a token").login = login;
        fact_value(&account_rows(&facts), "Login")
    };
    let held = login_row(LoginHere::Half(LoginHalf::Ok)).expect("CONTROL: a Login row");
    assert!(held.starts_with("signed in on this Mac"), "{held}");
    let absent = login_row(LoginHere::Absent).expect("a Login row");
    assert!(
        absent.starts_with("never signed in on this Mac"),
        "{absent}"
    );
    assert_eq!(login_row(LoginHere::Unread), None);
}

/// AN OUTSIDE LOGIN IS NEVER GIVEN AN END: nothing on this Mac knows when it ends, so the
/// card says it is signed in and no more, whatever the token read holds of a login.
#[test]
fn an_outside_login_is_never_given_an_end() {
    let mut facts = state_a(60);
    let mut token = facts.token.take().expect("a token");
    token.login = LoginHere::Half(LoginHalf::Ending { end: NOV_30 });
    facts.token = Some(token);
    facts.card.as_mut().expect("a card").login = None;
    let rows = account_rows(&facts);
    assert_eq!(
        fact_value(&rows, "Login").as_deref(),
        Some("signed in on this Mac \u{b7} this tab runs on it")
    );
    for text in words_of(&rows) {
        assert!(
            !text.contains("ends 30 Nov") && !text.contains("signed out"),
            "{text}"
        );
    }
    // CONTROL: a pool row's own login does say its end.
    let mut pool = state_b(60);
    pool.route = Some(AuthRoute::Stored);
    pool.card.as_mut().expect("a card").login = Some(LoginHalf::Ending { end: NOV_30 });
    let rows = account_rows(&pool);
    assert_eq!(
        nth_fact(&rows, "Login", 0),
        Some((
            "signed in \u{b7} ends 30 Nov \u{b7} this tab runs on it".into(),
            NoteTone::Warn
        ))
    );
}

/// The login's other words, and the tab's route.
#[test]
fn the_login_row_says_the_login_and_what_the_tab_runs_on() {
    let login = |half: Option<LoginHalf>, route: AuthRoute, enrolled: bool| {
        let mut facts = state_b(120);
        facts.route = Some(route);
        let card = facts.card.as_mut().expect("a card");
        card.login = half;
        if let Who::Pool { enrolled: was, .. } = &mut card.who {
            *was = enrolled;
        }
        lines(&super::build(&facts, 120, false))
    };
    let rows = login(Some(LoginHalf::InUse), AuthRoute::Outside, true);
    assert!(rows.contains(&"Login    not needed: the vault token signs this account in".into()));
    assert!(rows.contains(&"This tab runs on a key set outside aterm".into()));
    let rows = login(Some(LoginHalf::Ok), AuthRoute::Stored, true);
    assert!(rows.contains(&"Login    signed in on this Mac \u{b7} this tab runs on it".into()));
    // A tab on the stored login is told the login's own record, whatever token stands in for
    // it: `not needed: the vault token …` followed by `this tab runs on it` would read as the
    // tab running on the token. CONTROL: a tab on a key from outside is told the token does.
    for route in [AuthRoute::Stored, AuthRoute::Outside] {
        let mut facts = state_b(120);
        facts.route = Some(route);
        facts.token.as_mut().expect("a token").login = LoginHere::Half(LoginHalf::InUse);
        facts.card.as_mut().expect("a card").login = Some(LoginHalf::Ok);
        let rows = lines(&super::build(&facts, 120, false));
        let want = if route == AuthRoute::Stored {
            "Login    signed in on this Mac \u{b7} this tab runs on it"
        } else {
            "Login    not needed: the vault token signs this account in"
        };
        assert!(rows.contains(&want.to_owned()), "{route:?}: {rows:?}");
        assert_eq!(
            rows.iter()
                .filter(|row| row.contains("the vault token signs") && row.contains("runs on it"))
                .count(),
            0,
            "{route:?}: {rows:?}"
        );
    }
    let rows = login(
        Some(LoginHalf::Ended { at: Some(NOV_9) }),
        AuthRoute::Outside,
        true,
    );
    assert!(
        rows.contains(&"Login    signed out 9 Nov".into()),
        "{rows:?}"
    );
    let rows = login(
        Some(LoginHalf::Ended { at: None }),
        AuthRoute::Outside,
        true,
    );
    assert!(rows.contains(&"Login    signed out".into()));
    let rows = login(None, AuthRoute::Outside, false);
    assert!(rows.contains(&"Login    never signed in on this Mac".into()));
    let rows = login(None, AuthRoute::Outside, true);
    assert!(rows.contains(&"Login    not read yet".into()));
}

/// THE BUDGET VERDICT'S FIGURE IS THE FIRST SOURCE THAT HOLDS ONE, and a week never says a
/// bare percentage: the session's own, then the account reader's (this Mac's store, else the
/// journal, which says on which Mac and when it was read); with none, why.
#[test]
fn the_budget_verdict_takes_the_first_figure_held() {
    let budget = |facts: &FactMenuFacts| {
        let rows = super::build(facts, 120, false);
        (
            nth_fact(&rows, "Budget", 0).expect("the Budget verdict"),
            fact_value(&rows, "Read"),
        )
    };
    let mut facts = state_a(60);
    let seen = |by: Option<Mac>| Seen {
        figure: Figure::AtLeast(120),
        reset: Some(SAT_10),
        at: Some(TODAY_13_05),
        by,
    };
    facts.card.as_mut().expect("a card").budget.week = Some(seen(Some(mac("m15", false))));
    let ((value, _), read) = budget(&facts);
    assert_eq!(
        value, "84% left \u{b7} resets Sat 10:00",
        "the session's own"
    );
    assert_eq!(read, None, "an exact figure read here needs no Read row");
    (facts.own, facts.own_reset, facts.own_at) = (None, None, None);
    let ((value, _), read) = budget(&facts);
    assert_eq!(value, "\u{2264}88% left \u{b7} resets Sat 10:00");
    assert_eq!(read.as_deref(), Some("13:05 on m15"));
    facts.card.as_mut().expect("a card").budget.week = Some(seen(None));
    assert_eq!(budget(&facts).1.as_deref(), Some("13:05 on this Mac"));
    facts.card.as_mut().expect("a card").budget.week = None;
    assert_eq!(budget(&facts).0, ("not read".into(), NoteTone::Label));
    facts.weekly_gap = Some(WeeklyGap::TapOff);
    assert_eq!(
        budget(&facts).0,
        ("not read: tap off".into(), NoteTone::Label)
    );
    facts.own = Some(Figure::Out);
    facts.own_reset = Some(SAT_10);
    assert_eq!(
        budget(&facts).0,
        ("out until Sat 10:00".into(), NoteTone::Warn)
    );
    facts.own_reset = None;
    assert_eq!(budget(&facts).0, ("out".into(), NoteTone::Warn));
}

/// The Budget section's other rows, each only where it is held.
#[test]
fn the_budget_rows_say_credits_limits_and_a_long_models_name() {
    let mut facts = state_b(60);
    let card = facts.card.as_mut().expect("a card");
    card.budget.credits = Some(CreditsWord::InUse { since: TODAY_13_05 });
    card.budget.walls = vec![("Sonnet".into(), Some(NEXT_MON_09)), ("7d".into(), None)];
    card.budget.models[0].0 = "Fable Preview".into();
    card.budget.resets.as_mut().expect("an offer").left = 1;
    let rows = shown(&facts);
    for want in [
        "Credits  in use since 13:05",
        "Limits   Sonnet limit until Mon 09:00",
        "Limits   7d limit",
        "Model    Fable Preview week out until Mon 09:00",
        "Resets   1 limit reset left \u{b7} use by 30 Nov",
    ] {
        assert!(rows.contains(&want.to_owned()), "{want}: {rows:?}");
    }
    facts.credits = Some("out");
    let rows = account_rows(&facts);
    assert_eq!(
        nth_fact(&rows, "Credits", 0),
        Some(("out".into(), NoteTone::Warn))
    );
}

/// HISTORY IS LOOKED FOR BEFORE THE CARD SAYS NONE: an outside account whose readings this
/// Mac's series holds (found by its key) draws them as a pool row's; only an account with
/// nothing found says none, one whose rows hold no reading of its week says that and not
/// `none kept`, and a card not read yet says nothing of history at all.
#[test]
fn history_is_looked_for_by_key_before_the_card_says_none() {
    let mut facts = state_a(60);
    let history = state_b(60).card.expect("a card").history;
    facts.card.as_mut().expect("a card").history = history;
    let rows = account_rows(&facts);
    assert_eq!(
        fact_value(&rows, "History").as_deref(),
        Some("week left, read on this Mac since 18 Sep")
    );
    assert_eq!(
        rows.iter()
            .filter(|row| matches!(row, MenuRow::Plot { .. }))
            .count(),
        6,
        "both charts and the legend"
    );
    assert_eq!(
        fact_value(&account_rows(&state_a_debug(60)), "History").as_deref(),
        Some("none kept on this Mac for this account")
    );
    // Rows are kept (the account's plan, say) and none is a reading of its week: no caption
    // dates a week nothing read, and no chart is drawn.
    let mut planned = state_a_debug(60);
    planned.card.as_mut().expect("a card").history = Arc::new(History {
        since: Some(SEP_18),
        ..History::none(HistoryFrom::Nothing)
    });
    let rows = account_rows(&planned);
    assert_eq!(
        fact_value(&rows, "History").as_deref(),
        Some("no week reading on this Mac in the last 7 weeks")
    );
    assert!(!rows.iter().any(|row| matches!(row, MenuRow::Plot { .. })));
    assert_eq!(fact_value(&account_rows(&state_d1(60)), "History"), None);
    assert_eq!(
        fact_value(&account_rows(&state_d5(60)), "History").as_deref(),
        Some("week left, read on this Mac since 18 Sep"),
        "a failed pool read keeps the card's history"
    );
    // Fewer than two days held: the eight days alone.
    let mut young = state_b(60);
    let mut history = (*young.card.as_ref().expect("a card").history).clone();
    history.days.truncate(1);
    young.card.as_mut().expect("a card").history = Arc::new(history);
    let rows = account_rows(&young);
    assert_eq!(
        rows.iter()
            .filter(|row| matches!(row, MenuRow::Plot { .. }))
            .count(),
        4
    );
}

/// A CLAIM ELSEWHERE SAYS IN USE AND WHEN IT BEGAN: a claim outlives use, so the card never
/// says a session is running there; a holder with no lab name is said as one.
#[test]
fn a_claim_elsewhere_says_in_use_and_when_it_began() {
    let rows = account_rows(&state_c(60, true));
    assert_eq!(
        fact_value(&rows, "m22").as_deref(),
        Some("in use \u{b7} claimed since 09:40")
    );
    for text in words_of(&rows) {
        assert!(!text.contains("running"), "{text}");
    }
    // A second Mac that holds it, and one with no lab name.
    let mut facts = state_b(60);
    facts.card.as_mut().expect("a card").machine.claims = Claims::Read(vec![
        holder(mac("m20", true), TODAY_09_40),
        holder(mac("m15", false), TODAY_13_05),
        holder(mac("", false), TODAY_09_40),
    ]);
    let rows = shown(&facts);
    assert_eq!(
        rows[4],
        "Machine  home this Mac \u{b7} in use here and on 2 other Macs"
    );
    assert!(rows.contains(&"m15      in use \u{b7} claimed since 13:05".to_owned()));
    assert!(
        rows.contains(&"Unnamed  a Mac with no lab name, claimed since 09:40".to_owned()),
        "{rows:?}"
    );
    assert!(!rows.iter().any(|row| row.starts_with("Others")));
    facts.card.as_mut().expect("a card").machine.claims = Claims::Read(vec![
        holder(mac("m20", true), TODAY_09_40),
        holder(mac("m15", false), TODAY_13_05),
    ]);
    assert_eq!(
        shown(&facts)[4],
        "Machine  home this Mac \u{b7} in use here and on m15"
    );
}

/// NO CLAIMS TABLE SAYS NOT KNOWN, NEVER NOT IN USE: a Mac that reads no other Mac, a table
/// that could not be read, and a program whose accounts are not tracked each say so.
#[test]
fn no_claims_table_says_not_known_never_not_in_use() {
    for (claims, verdict, row) in [
        (
            Claims::Absent,
            "home m22 \u{b7} use there not known",
            "not known: this Mac does not read other Macs",
        ),
        (
            Claims::Unreadable,
            "home m22 \u{b7} use there not known",
            "not known: other Macs could not be read just now",
        ),
        (
            Claims::NotTracked,
            "home m22 \u{b7} use there not tracked",
            "not tracked for Claude Code accounts",
        ),
    ] {
        let mut facts = state_c(60, false);
        let machine = &mut facts.card.as_mut().expect("a card").machine;
        machine.claims = claims;
        let rows = account_rows(&facts);
        assert_eq!(
            nth_fact(&rows, "Machine", 0).expect("the verdict").0,
            verdict
        );
        assert_eq!(fact_value(&rows, "m22").as_deref(), Some(row));
        for text in words_of(&rows) {
            assert!(!text.contains("not in use"), "{text}");
        }
    }
    // The same unknowns under `Others`, where the home is this Mac.
    let mut facts = state_b(60);
    facts.card.as_mut().expect("a card").machine.claims = Claims::Unreadable;
    assert_eq!(
        fact_value(&account_rows(&facts), "Others").as_deref(),
        Some("not known: other Macs could not be read just now")
    );
    // A lease this Mac holds says nothing of the others either.
    // A lease this Mac holds says nothing of the others either, and keeps why no table spoke.
    for (why, words) in [
        (
            NoTable::Absent,
            "not known: this Mac does not read other Macs",
        ),
        (
            NoTable::Unreadable,
            "not known: other Macs could not be read just now",
        ),
        (
            NoTable::NotAsked,
            "not known: this account has no address on record",
        ),
    ] {
        facts.card.as_mut().expect("a card").machine.claims =
            Claims::Senior(holder(mac("m20", true), TODAY_09_40), why);
        assert_eq!(
            fact_value(&account_rows(&facts), "Others").as_deref(),
            Some(words)
        );
    }
}

/// A HOLD FOR A MAC WHOSE CLOCK DISAGREES IS NOT USE. The home Mac this Mac only holds the
/// account for is listed as held, with no `claimed since` and no time made of the hold's
/// sentinel; the verdict says its use is not known, on the card and in the clipboard report.
/// Negative control: the same Mac with a claim of its own is in use since that claim began.
#[test]
fn a_hold_for_a_skewed_mac_is_not_said_as_use() {
    let mut facts = state_c(60, true);
    let claimed = account_rows(&facts);
    assert_eq!(
        fact_value(&claimed, "m22").as_deref(),
        Some("in use \u{b7} claimed since 09:40"),
        "CONTROL: a claim says when it began"
    );
    let held = Holder {
        mac: mac("m22", false),
        since: None,
        until: TODAY_21_40,
    };
    facts.card.as_mut().expect("a card").machine.claims =
        Claims::Read(vec![holder(mac("m20", true), TODAY_13_05), held.clone()]);
    let rows = account_rows(&facts);
    assert_eq!(
        nth_fact(&rows, "Machine", 0).expect("the verdict").0,
        "home m22 \u{b7} use there not known"
    );
    assert_eq!(
        fact_value(&rows, "m22").as_deref(),
        Some("held for it: its clock disagrees, use not known")
    );
    let report = details_report(&facts);
    assert!(
        report.contains("Held for m22 while its clock disagrees, until 21:40"),
        "{report}"
    );
    for text in words_of(&rows)
        .into_iter()
        .chain(report.lines().map(str::to_owned))
    {
        assert!(
            !text.contains("-2922"),
            "the hold's sentinel as a date: {text}"
        );
        if text.contains("m22") {
            assert!(!text.contains("claimed since"), "{text}");
        }
    }
    // Where the home is this Mac, a hold for another Mac is no `in use … on` either.
    let mut facts = state_b(60);
    facts.card.as_mut().expect("a card").machine.claims = Claims::Read(vec![held]);
    assert_eq!(
        nth_fact(&account_rows(&facts), "Machine", 0)
            .expect("the verdict")
            .0,
        "home this Mac \u{b7} in use here"
    );
}

/// AN ACCOUNT NOTHING NAMES IS NOT KNOWN, NEVER NONE. A card whose account has no address was
/// not looked up in the pool file, the claims table or the series, and each row says that: no
/// `no home on record`, no `no other Mac is using it`, no `none kept`. Negative control: the
/// same card over a named account that was looked up and not found says those words.
#[test]
fn an_account_nothing_names_says_not_known_never_none() {
    let none = [
        "no home on record",
        "no other Mac is using it",
        "none kept on this Mac for this account",
    ];
    let mut facts = state_a_debug(60);
    {
        let card = facts.card.as_mut().expect("a card");
        card.machine.home = Home::NotListed;
        card.machine.claims = Claims::Read(Vec::new());
        card.history = Arc::new(History::none(HistoryFrom::Nothing));
    }
    let found = words_of(&account_rows(&facts)).join("\n");
    for words in none {
        assert!(found.contains(words), "CONTROL: {words} in {found}");
    }
    {
        let card = facts.card.as_mut().expect("a card");
        card.who = Who::Unknown;
        card.machine.home = Home::NotAsked;
        card.machine.claims = Claims::NotAsked;
        card.history = Arc::new(History::none(HistoryFrom::NotRead));
    }
    let rows = account_rows(&facts);
    let text = words_of(&rows).join("\n");
    for words in none {
        assert!(!text.contains(words), "{words} in {text}");
    }
    assert_eq!(
        nth_fact(&rows, "Machine", 0).expect("the verdict").0,
        "home not known \u{b7} in use here"
    );
    let unnamed = "not known: this tab's account is not named yet";
    assert_eq!(fact_value(&rows, "Home").as_deref(), Some(unnamed));
    assert_eq!(fact_value(&rows, "Others").as_deref(), Some(unnamed));
    assert_eq!(
        fact_value(&rows, "History").as_deref(),
        Some("not read: this tab's account is not named yet")
    );
    // The tabs on the account cannot be counted either: no `only` is claimed.
    facts.tabs_here = None;
    assert_eq!(
        fact_value(&account_rows(&facts), "This Mac").as_deref(),
        Some("in use: this tab")
    );
}

/// USUAL SAYS THE DATE ITS COUNT STARTS FROM, counts starts and says "starts"; with no journal
/// held, or no start counted, there is no row.
#[test]
fn usual_says_the_date_its_count_starts_from() {
    let rows = account_rows(&state_b(60));
    assert_eq!(
        fact_value(&rows, "Usual").as_deref(),
        Some("this Mac \u{b7} 40 of 41 starts since 28 Oct")
    );
    for journal in [
        JournalHeld::Off,
        JournalHeld::NotReadYet,
        JournalHeld::Silent,
    ] {
        let mut facts = state_b(60);
        facts.card.as_mut().expect("a card").machine.journal = journal;
        assert_eq!(
            fact_value(&account_rows(&facts), "Usual"),
            None,
            "{journal:?}"
        );
    }
    let mut facts = state_b(60);
    facts.card.as_mut().expect("a card").machine.starts = None;
    assert_eq!(fact_value(&account_rows(&facts), "Usual"), None);
}

/// THE ROTATION SENTENCE COVERS EVERY PHASE AND STAY WORD: one sentence, the first condition
/// that applies.
#[test]
fn rotation_sentence_covers_every_phase_and_stay_word() {
    let sentence = |facts: &FactMenuFacts| {
        let rows = super::build(facts, 200, false);
        let at = rows
            .iter()
            .position(
                |row| matches!(row, MenuRow::Section(title) if title == "Automatic switching"),
            )
            .expect("the section");
        match &rows[at + 1] {
            MenuRow::Note { text, .. } => Some(text.clone()),
            _ => None,
        }
    };
    let with = |edit: &dyn Fn(&mut AccountProgress)| {
        let mut facts = state_b(60);
        edit(facts.account_progress.as_mut().expect("progress"));
        sentence(&facts)
    };
    let phase = |phase: ProgressPhase| with(&|p| p.phase = phase.clone());
    let stay = |word: &str| with(&|p| p.stay = Some(word.into()));
    assert_eq!(
        sentence(&state_a(60)).as_deref(),
        Some("Off for this tab: its login is not one of this Mac's 3 accounts.")
    );
    for (size, want) in [
        (0, "Off for this tab: this Mac lists no accounts."),
        (
            1,
            "Off for this tab: its login is not this Mac's one account.",
        ),
    ] {
        let mut facts = state_a(60);
        facts.card.as_mut().expect("a card").pool_size = size;
        assert_eq!(sentence(&facts).as_deref(), Some(want));
    }
    assert_eq!(
        sentence(&state_c(60, true)).as_deref(),
        Some("m22 holds this account until 21:40.")
    );
    let mut unnamed = state_c(60, true);
    unnamed.card.as_mut().expect("a card").machine.claims = Claims::Absent;
    assert_eq!(
        sentence(&unnamed).as_deref(),
        Some("Another Mac holds this account until 21:40.")
    );
    assert_eq!(
        with(&|p| {
            p.own_claim = Some(OwnClaimFinding {
                blocker: OwnClaimBlocker::CannotRecord,
                pinned: false,
            });
        })
        .as_deref(),
        Some("This Mac cannot record its claim on this account.")
    );
    let off = "Off: automatic switching is turned off.";
    assert_eq!(phase(ProgressPhase::Disabled).as_deref(), Some(off));
    assert_eq!(stay("off").as_deref(), Some(off));
    assert_eq!(
        stay("pinned").as_deref(),
        Some("Off while this Mac is pinned to this account.")
    );
    for word in ["other-home", "not-a-stored-login", "keyring-store"] {
        assert_eq!(
            stay(word).as_deref(),
            Some("Off for this tab: it keeps its own login."),
            "{word}"
        );
    }
    // Any other stay word ranks under the phase. The host sets `nothing-due` at every ordinary
    // turn end: a healthy tab still says what switching can do, and a limited one its limit.
    for word in ["nothing-due", "no-candidate", "probe: due", "jobs"] {
        assert_eq!(
            stay(word).as_deref(),
            Some("On: 1 of this Mac's 2 accounts can take over."),
            "{word}"
        );
        for (phase, want) in [
            (ProgressPhase::Paused, "Paused for this tab."),
            (ProgressPhase::Limited, "At its limit."),
            (ProgressPhase::Waiting, "At its limit."),
        ] {
            assert_eq!(
                with(&|p| {
                    p.phase = phase.clone();
                    p.stay = Some(word.into());
                })
                .as_deref(),
                Some(want),
                "{word}"
            );
        }
    }
    assert_eq!(
        with(&|p| {
            p.phase = ProgressPhase::Unknown;
            p.stay = Some("jobs".into());
        })
        .as_deref(),
        Some("Waiting until restart is safe."),
        "where the phase says nothing was read, the host's own sentence for the word"
    );
    assert_eq!(
        phase(ProgressPhase::Paused).as_deref(),
        Some("Paused for this tab.")
    );
    assert_eq!(
        phase(ProgressPhase::Watching).as_deref(),
        Some("On: 1 of this Mac's 2 accounts can take over.")
    );
    assert_eq!(
        with(&|p| p.eligible = Some(0)).as_deref(),
        Some("On, but no other account can take over now.")
    );
    for limited in [ProgressPhase::Limited, ProgressPhase::Waiting] {
        assert_eq!(phase(limited.clone()).as_deref(), Some("At its limit."));
        assert_eq!(
            with(&|p| {
                p.phase = limited.clone();
                p.next_at = Some(TODAY_20_10);
            })
            .as_deref(),
            Some("At its limit. aterm looks again 20:10.")
        );
    }
    let to = || "sol.m20.2@proton.me".to_owned();
    assert_eq!(
        phase(ProgressPhase::Switching { to: to() }).as_deref(),
        Some("Moving this tab to sol.m20.2@proton.me.")
    );
    assert_eq!(
        phase(ProgressPhase::Verifying { to: to() }).as_deref(),
        Some("Checking sol.m20.2@proton.me before the move.")
    );
    assert_eq!(
        phase(ProgressPhase::SigningIn {
            label: to(),
            route: None
        })
        .as_deref(),
        Some("Signing in sol.m20.2@proton.me.")
    );
    assert_eq!(
        phase(ProgressPhase::Probing).as_deref(),
        Some("Checking this Mac's accounts.")
    );
    assert_eq!(
        phase(ProgressPhase::Ready).as_deref(),
        Some("Another account is ready.")
    );
    assert_eq!(
        phase(ProgressPhase::Unknown).as_deref(),
        Some("Not read yet.")
    );
    // No observation of a pool account: no sentence, and no Network State item.
    let mut quiet = state_b(60);
    quiet.account_progress = None;
    assert_eq!(sentence(&quiet), None);
    assert!(
        !shown(&quiet)
            .iter()
            .any(|row| row.contains("Network State"))
    );
}

/// THE PIN ITEM IS NEVER CHECKED AND SAYS WHAT A PRESS DOES: it unpins and moves to the next
/// account, pinned or not; the fact above it says whether this Mac is pinned and to whom.
#[test]
fn the_pin_item_is_never_checked_and_says_what_a_press_does() {
    for (pinned, fact, label, spoken) in [
        (
            None,
            "none: aterm picks the account",
            "Move this Mac to the next account",
            "Move this Mac's Claude Code sessions to the next account",
        ),
        (
            Some("sol.m20.2@proton.me"),
            "this Mac stays on sol.m20.2@proton.me",
            "Unpin and move to the next account",
            "Unpin this Mac and move its Claude Code sessions to the next account",
        ),
        (
            Some(""),
            "this Mac stays on one account",
            "Unpin and move to the next account",
            "Unpin this Mac and move its Claude Code sessions to the next account",
        ),
    ] {
        let mut facts = state_b(60);
        facts.pinned = pinned.map(str::to_owned);
        let rows = account_rows(&facts);
        assert_eq!(fact_value(&rows, "Pin").as_deref(), Some(fact));
        let items: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                MenuRow::Item {
                    act: MenuAct::Fact(Fact::Account, FactAct::AutoRotate),
                    label,
                    spoken,
                    checked,
                    enabled,
                    ..
                } => Some((label.as_str(), spoken.as_str(), *checked, *enabled)),
                _ => None,
            })
            .collect();
        assert_eq!(items, [(label, spoken, false, true)]);
    }
    let mut facts = state_b(60);
    facts.manual_signin_unavailable = true;
    let rows = account_rows(&facts);
    assert!(rows.iter().any(|row| matches!(
        row,
        MenuRow::Item {
            act: MenuAct::Fact(Fact::Account, FactAct::AutoRotate),
            enabled: false,
            checked: false,
            ..
        }
    )));
}

/// A POOL ITEM KEEPS ITS TRAIL OR MOVES IT UNDER: the whole address always; its standing at
/// the row's far end where both fit, else on its own row under the name; in a room that does
/// not hold the address it breaks before its `@`.
#[test]
fn a_pool_item_keeps_its_trail_or_moves_it_under() {
    let long = acct(
        "long",
        "a.rather.long.account.name@example.proton.me",
        "not signed in",
        false,
    );
    let item = |room: usize| {
        let mut out = Vec::new();
        account_item(&mut out, &long, room);
        for row in &out {
            assert!(row_width(row) <= room, "{room}: {row:?}");
        }
        lines(&out)
    };
    assert_eq!(
        item(64),
        ["\u{2386}  a.rather.long.account.name@example.proton.me  [not signed in]"]
    );
    assert_eq!(
        item(60),
        [
            "\u{2386}  a.rather.long.account.name@example.proton.me",
            "   not signed in"
        ]
    );
    assert_eq!(
        item(44),
        [
            "\u{2386}  a.rather.long.account.name",
            "   @example.proton.me",
            "   not signed in"
        ]
    );
    assert_eq!(
        item(30),
        [
            "\u{2386}  a.rather.long.account.nam",
            "   e@example.proton.me",
            "   not signed in"
        ],
        "a name part wider than the room breaks at the cell, nothing dropped"
    );
    // The item is the choice whatever the room: its act, its check and its words to a reader.
    let mut out = Vec::new();
    let mine = acct("sol-1", "sol.m20.1@proton.me", "61% left", true);
    account_item(&mut out, &mine, 30);
    assert!(matches!(
        &out[0],
        MenuRow::Item {
            act: MenuAct::Fact(Fact::Account, FactAct::UseAccount(label)),
            checked: true,
            enabled: true,
            spoken,
            ..
        } if label.as_str() == "sol-1" && spoken == "sol.m20.1@proton.me, 61% left"
    ));
    assert_eq!(
        lines(&out),
        ["\u{2386}  sol.m20.1@proton.me \u{25cf}", "   61% left"]
    );
}

/// COPY DETAILS HOLDS EVERY FACT, EMAILS AND BOTH CLOCKS: the card's facts unwrapped, what the
/// card leaves out, each time as local words with its RFC 3339 instant, and the command that
/// prints older days.
#[test]
fn copy_details_holds_every_fact_emails_and_both_clocks() {
    let mut facts = state_c(60, true);
    let token = facts.token.as_mut().expect("a token");
    token.made_at = u64::try_from(OCT_2).ok();
    token.verified_at = u64::try_from(OCT_12).ok();
    token.made_on = Some("m20".into());
    token.vault_words = Some("in the vault, shared with 2 Macs".into());
    token.peers = vec!["m15".into(), "m22".into()];
    token.peers_read_at = u64::try_from(TODAY_13_05).ok();
    let progress = facts.account_progress.as_mut().expect("progress");
    progress.details = vec!["sol.m20.2@proton.me: no usable sign-in route".into(); 3];
    progress.next_at = Some(TODAY_20_10);
    let before = facts.clone();
    let report = details_report(&facts);
    assert_eq!(facts, before, "the report changes nothing");
    for want in [
        "Account: sol.m20.1@proton.me",
        "Token: in the vault \u{b7} good until 1 Feb 2027 (2027-02-01T12:00:00Z)",
        "Budget: 61% left \u{b7} resets Sat 10:00 (2026-11-14T10:00:00Z)",
        "Machine: home m22 \u{b7} in use there and here",
        "Login: not needed: this tab runs on the vault token",
        "Plan: Max 20x",
        "5-hour: 40% left \u{b7} resets 20:10 (2026-11-11T20:10:00Z)",
        "Fable: week out until Mon 09:00 (2026-11-16T09:00:00Z)",
        "Home: m22 \u{b7} set on the accounts sheet since 28 Oct (2026-10-28T12:00:00Z) \u{b7} also assigned to this Mac",
        "m22: in use \u{b7} claimed since 09:40 (2026-11-11T09:40:00Z)",
        "Automatic switching: m22 holds this account until 21:40 (2026-11-11T21:40:00Z).",
        "Token made: Fri 2 Oct 12:00 (2026-10-02T12:00:00Z)",
        "Token ends: Mon 1 Feb 12:00 (2027-02-01T12:00:00Z)",
        "Token made on: m20",
        "Vault: in the vault, shared with 2 Macs",
        "Other Macs on the token: m15, m22, read 13:05 (2026-11-11T13:05:00Z)",
        "Assigned to: m22, this Mac (m20)",
        "Claim: m22, since 09:40 (2026-11-11T09:40:00Z), until 21:40 (2026-11-11T21:40:00Z)",
        "Claim: this Mac (m20), since 13:05",
        "Starts since",
        "m22 28, this Mac (m20) 2",
        "Next event 20:10 (2026-11-11T20:10:00Z)",
        "Older days: aterm harness accounts history sol-1",
    ] {
        assert!(report.contains(want), "{want}\n{report}");
    }
    assert_eq!(
        report
            .matches("sol.m20.2@proton.me: no usable sign-in route")
            .count(),
        3,
        "every recovery detail, however many"
    );
    assert!(
        report
            .lines()
            .last()
            .is_some_and(|line| line.starts_with("Older days:")),
        "{report}"
    );
    assert!(
        !report.contains('\u{2581}') && !report.contains('\u{2588}'),
        "no chart"
    );
    // A login outside the pool has no roster label the history command takes: the report names
    // no command it could not run for this account.
    let outside = details_report(&state_a(60));
    assert!(
        outside.contains("Account: unicorn.m15.2@proton.me"),
        "{outside}"
    );
    assert!(!outside.contains("Older days:"), "{outside}");
    // An unknown clock is not an epoch date.
    let mut unknown = state_b(60);
    let progress = unknown.account_progress.as_mut().expect("progress");
    (progress.observed_at, progress.next_at) = (0, Some(0));
    let report = details_report(&unknown);
    assert!(report.contains("Not observed yet") && report.contains("No next event scheduled"));
    assert!(!report.contains("1970-"), "{report}");
    // Why the week is not read is in the report, whole.
    let mut gap = state_d1(60);
    (gap.own, gap.weekly_gap) = (None, Some(WeeklyGap::TapOff));
    assert!(details_report(&gap).contains("usage tap"), "the tap's why");
}

fn chart(rows: &[MenuRow], lead_word: &str) -> Option<usize> {
    rows.iter().position(|row| match row {
        MenuRow::Plot { cells, .. } => {
            let text: String = cells.iter().take(FACT_LABEL_CELLS).map(|c| c.ch).collect();
            text.trim_end() == lead_word
        }
        _ => false,
    })
}

fn plot_cells(row: &MenuRow) -> &[PlotCell] {
    match row {
        MenuRow::Plot { cells, .. } => cells,
        other => panic!("not a chart row: {other:?}"),
    }
}

/// THE EIGHT-DAY BAND MARKS A RESET, AN OUT AND A GAP. One cell is four hours at 60 cells and
/// the last cell is now's; a cell draws the last reading in it, on two rows of eighths; the
/// lower row marks the cell a new week began in (`↺`), a reading of 0 left (`╳`) and a cell
/// with no reading after the first held (`┄`), each in its own ink; before the first reading
/// the band is blank, and nothing is drawn between readings. Its first row is its name and
/// its axis, which names each day at its midnight, and the lower row carries what a reader is
/// told.
#[test]
fn the_eight_day_band_marks_a_reset_an_out_and_a_gap() {
    let rows = account_rows(&state_a(60));
    let at = chart(&rows, "8 days").expect("the eight-day chart");
    let (axis, upper, lower) = (
        plot_cells(&rows[at]),
        plot_cells(&rows[at + 1]),
        plot_cells(&rows[at + 2]),
    );
    assert_eq!(
        upper.len(),
        FACT_LABEL_CELLS + 48,
        "six cells a day of eight"
    );
    assert_eq!((axis.len(), lower.len()), (upper.len(), upper.len()));
    let band =
        |cells: &[PlotCell]| -> String { cells[FACT_LABEL_CELLS..].iter().map(|c| c.ch).collect() };
    let (top, bottom) = (band(upper), band(lower));
    let (top, bottom): (Vec<char>, Vec<char>) = (top.chars().collect(), bottom.chars().collect());
    // Thirty four-hour steps end at now: the first reading is 29 cells back.
    let first = 48 - 30;
    assert!(
        top[..first]
            .iter()
            .chain(&bottom[..first])
            .all(|ch| *ch == ' '),
        "blank before the first reading"
    );
    // Step 0 read 50% used: half the band, the lower row full and the upper empty.
    assert_eq!((top[first], bottom[first]), (' ', '\u{2588}'));
    // Step 19 read 97.5% used: one eighth at most.
    assert_eq!((top[first + 19], bottom[first + 19]), (' ', '\u{2581}'));
    assert_eq!(bottom[first + 20], '\u{2573}', "0 left");
    assert_eq!(bottom[first + 21], '\u{21ba}', "the new week's first cell");
    assert_eq!(top[first + 22], '\u{2588}', "a fresh week is nearly whole");
    assert_eq!(
        bottom[first + 24..first + 27],
        ['\u{2504}'; 3],
        "three cells with no reading"
    );
    assert_ne!(bottom[47], ' ', "the last cell is now's");
    let ink = |cells: &[PlotCell], ch: char| {
        cells
            .iter()
            .find(|cell| cell.ch == ch)
            .map(|cell| cell.ink)
            .unwrap_or_else(|| panic!("no {ch:?}"))
    };
    assert_eq!(ink(lower, '\u{2573}'), PlotInk::Warn);
    assert_eq!(ink(lower, '\u{21ba}'), PlotInk::Mark);
    assert_eq!(ink(lower, '\u{2504}'), PlotInk::Track);
    assert_eq!(ink(lower, '\u{2588}'), PlotInk::Read);
    assert_eq!(
        ink(axis, '8'),
        PlotInk::Value,
        "the chart's name is in the fact column, in its ink"
    );
    assert!(
        upper[..FACT_LABEL_CELLS]
            .iter()
            .chain(&lower[..FACT_LABEL_CELLS])
            .all(|cell| cell.ch == ' '),
        "the band's own rows name nothing"
    );
    // The axis: a weekday at each local midnight's cell, a day six cells wide; now is 18:20
    // on Wednesday, so Wednesday's midnight is four cells before the last.
    let days: String = band(axis);
    assert_eq!(days.find("Wed").map(|at| at % 6), Some((48 - 5) % 6));
    assert_eq!(&days[43..46], "Wed");
    assert_eq!(&days[37..40], "Tue");
    assert!(
        axis[FACT_LABEL_CELLS..]
            .iter()
            .all(|cell| cell.ink == PlotInk::Axis)
    );
    let spoken = |row: &MenuRow| match row {
        MenuRow::Plot { spoken, .. } => spoken.clone(),
        _ => String::new(),
    };
    assert_eq!(spoken(&rows[at]), "");
    assert_eq!(spoken(&rows[at + 1]), "");
    let said = spoken(&rows[at + 2]);
    assert!(
        said.starts_with(
            "Week left over the last 8 days, as the lab's Macs read it: 50 percent left on "
        ) && said.contains(", a new week from ")
            && said.ends_with(" now"),
        "{said}"
    );
    // A last reading that is not in now's cell is said with when it was made, never as `now`.
    let mut stale = state_a(60);
    let mut history = (*stale.card.as_ref().expect("a card").history).clone();
    history.reads.truncate(history.reads.len() - 3);
    let last = history.reads.last().expect("a reading").t;
    stale.card.as_mut().expect("a card").history = Arc::new(history);
    let rows_stale = account_rows(&stale);
    let at_stale = chart(&rows_stale, "8 days").expect("the eight-day chart");
    let said = spoken(&rows_stale[at_stale + 2]);
    assert!(
        said.ends_with(&format!(
            " percent left on {}",
            outlook::spoken_when(last, NOW, &Offsets::fixed(0))
        )) && !said.contains(" now"),
        "{said}"
    );
    // The legend is the last chart row, its marks in the inks they are drawn in.
    let legend = plot_cells(&rows[at + 3]);
    assert_eq!(ink(legend, '\u{2573}'), PlotInk::Warn);
    assert_eq!(ink(legend, '\u{21ba}'), PlotInk::Mark);
    assert_eq!(ink(legend, '\u{2504}'), PlotInk::Track);
    // Four cells a day at 44 cells, three at 33.
    for (room, cells) in [(44, 32), (33, 24)] {
        let rows = account_rows(&state_a(room));
        let at = chart(&rows, "8 days").expect("the eight-day chart");
        assert_eq!(
            plot_cells(&rows[at]).len(),
            FACT_LABEL_CELLS + cells,
            "{room}"
        );
    }
}

/// THE WEEKS ROW DRAWS WHOLE WEEKS ENDING TODAY: one cell a local day, seven weeks at 60
/// cells, five at 44, three at 33; a cell is what was left at the day's end in eighths, `╳` a
/// day that ended at 0 left, `┄` a day with no reading, blank before the oldest day held; its
/// first row is its name and its axis, which dates each week of cells, so a row of words
/// parts its cells from the eight-day band's.
#[test]
fn the_weeks_row_draws_whole_weeks_ending_today() {
    for (room, weeks) in [(60, 7), (44, 5), (33, 3)] {
        let rows = account_rows(&state_b(room));
        let at = chart(&rows, &format!("{weeks} weeks")).expect("the weeks row");
        let (axis, row) = (plot_cells(&rows[at]), plot_cells(&rows[at + 1]));
        assert!(
            chart(&rows, "8 days").is_some_and(|band| band + 3 == at),
            "{room}: the weeks' name and dates part the two charts' cells"
        );
        let cells: Vec<char> = row[FACT_LABEL_CELLS..].iter().map(|c| c.ch).collect();
        assert_eq!(cells.len(), weeks * 7, "{room}");
        let back = |days: usize| cells[cells.len() - 1 - days];
        assert_eq!(back(0), '\u{2588}', "{room}: today, a whole week left");
        assert_eq!(back(1), '\u{2588}', "{room}: 88% left is the top eighth");
        assert_eq!(back(6), '\u{2583}', "{room}: 28% left");
        assert_eq!(back(15), '\u{2504}', "{room}: a day with no reading");
        assert_eq!(back(17), '\u{2573}', "{room}: a day that ended out");
        let dates: String = axis[FACT_LABEL_CELLS..].iter().map(|c| c.ch).collect();
        let first = MIDNIGHT - i64::try_from(weeks * 7 - 1).unwrap_or(0) * DAY;
        let want = outlook::date_words(first, NOW, &Offsets::fixed(0));
        assert!(dates.starts_with(&want), "{room}: {dates:?} begins {want}");
        assert_eq!(&dates[dates.len() - 7..].trim_end(), &"5 Nov", "{room}");
        let said = match &rows[at + 1] {
            MenuRow::Plot { spoken, .. } => spoken.clone(),
            _ => String::new(),
        };
        assert!(
            said.starts_with(&format!(
                "Week left at each day's end, {} days to today",
                weeks * 7
            )),
            "{said}"
        );
    }
    let rows = account_rows(&state_b(60));
    let at = chart(&rows, "7 weeks").expect("the weeks row");
    let said = match &rows[at + 1] {
        MenuRow::Plot { spoken, .. } => spoken.clone(),
        _ => String::new(),
    };
    assert_eq!(
        said,
        "Week left at each day's end, 49 days to today: 2 days out, 1 day not read"
    );
    assert!(
        lines(&rows).contains(&"5-hour limit reached on 3 of the 49 days read".to_owned()),
        "the days a five-hour limit was reached, of the days that hold a reading"
    );
    // Blank before the oldest day held.
    let mut young = state_b(60);
    let mut history = (*young.card.as_ref().expect("a card").history).clone();
    history.days = series_days(10);
    young.card.as_mut().expect("a card").history = Arc::new(history);
    let rows = account_rows(&young);
    let at = chart(&rows, "7 weeks").expect("the weeks row");
    let cells = &plot_cells(&rows[at + 1])[FACT_LABEL_CELLS..];
    assert!(cells[..39].iter().all(|cell| cell.ch == ' '));
    assert!(cells[39..].iter().all(|cell| cell.ch != ' '));
    // The note counts the days read, never the blank cells before them.
    assert!(
        lines(&rows).contains(&"5-hour limit reached on 2 of the 10 days read".to_owned()),
        "{:?}",
        lines(&rows)
    );
    // The journal holds no day rows: an account read from it draws no weeks.
    assert_eq!(chart(&account_rows(&state_a(60)), "7 weeks"), None);
}

/// UNDER 33 CELLS HISTORY IS WORDS: no chart fits, so the span is said: the oldest figure held
/// and its date, and the newest.
#[test]
fn under_33_cells_history_is_words() {
    for room in [20, 24, 32] {
        let rows = account_rows(&state_b(room));
        assert!(
            !rows.iter().any(|row| matches!(row, MenuRow::Plot { .. })),
            "{room}: no chart"
        );
        let said = lines(&rows).join(" ");
        let said = said.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            said.contains("from 50% left on 6 Nov to 84% left now"),
            "{room}: {said}"
        );
    }
    assert!(
        account_rows(&state_b(33))
            .iter()
            .any(|row| matches!(row, MenuRow::Plot { .. })),
        "33 cells hold the charts"
    );
    let history_of = |facts: &FactMenuFacts| {
        let said = lines(&account_rows(facts)).join(" ");
        said.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    // The newest figure is `now` only when it was read within the hour: an older one is said
    // with its date.
    let mut stale = state_b(24);
    let mut history = (*stale.card.as_ref().expect("a card").history).clone();
    history.reads.truncate(history.reads.len() - 3);
    stale.card.as_mut().expect("a card").history = Arc::new(history.clone());
    let said = history_of(&stale);
    assert!(
        said.contains("from 50% left on 6 Nov to 96% left on 10 Nov"),
        "{said}"
    );
    assert!(!said.contains("left now"), "{said}");
    // With no reading in the last eight days the span is the day rows', each with its date.
    history.reads.clear();
    history.days = series_days(40).into_iter().take(10).collect();
    stale.card.as_mut().expect("a card").history = Arc::new(history);
    let said = history_of(&stale);
    assert!(said.contains(" left on 3 Oct to "), "{said}");
    assert!(said.contains(" left on 12 Oct"), "{said}");
    assert!(!said.contains("left now"), "{said}");
}

/// The wrap the card's rows are made with: a ` · ` is the first place a row ends, then a blank,
/// then before an `@`, then the cell; a part that needs two rows starts on the one before it;
/// nothing is dropped but the joins.
#[test]
fn wrap_cells_breaks_at_joins_then_blanks_then_the_at_sign() {
    assert_eq!(wrap_cells("a \u{b7} b", 20), ["a \u{b7} b"]);
    assert_eq!(
        wrap_cells("signed in on this Mac \u{b7} this tab runs on it", 35),
        ["signed in on this Mac", "this tab runs on it"]
    );
    assert_eq!(
        wrap_cells("m15 \u{b7} set on the accounts sheet since 2 Oct", 35),
        ["m15 \u{b7} set on the accounts sheet", "since 2 Oct"]
    );
    // A long part's first word that does not fit after the dot starts its own row.
    assert_eq!(
        wrap_cells("not in use here \u{b7} set on the accounts sheet", 18),
        ["not in use here", "set on the", "accounts sheet"]
    );
    assert_eq!(
        wrap_cells("unicorn.m15.2@proton.me", 17),
        ["unicorn.m15.2", "@proton.me"]
    );
    assert_eq!(wrap_cells("abcdefghij", 4), ["abcd", "efgh", "ij"]);
    assert_eq!(wrap_cells("", 10), Vec::<String>::new());
    for cells in 1..40 {
        let text = "Off for this tab: its login is not one of this Mac's 3 accounts.";
        let rows = wrap_cells(text, cells);
        assert!(
            rows.iter().all(|row| display_cells(row) <= cells),
            "{cells}"
        );
        assert_eq!(
            rows.concat().replace(' ', ""),
            text.replace(' ', ""),
            "{cells}: every letter kept"
        );
    }
}

/// A fact names itself once to a reader, whole, however many rows its value takes; a row that
/// continues it says nothing more.
#[test]
fn a_wrapped_fact_is_spoken_once_and_whole() {
    let rows = account_rows(&state_a_debug(44));
    let home: Vec<&MenuRow> = rows
        .iter()
        .skip_while(|row| !matches!(row, MenuRow::Fact { label, .. } if label == "Home"))
        .take(2)
        .collect();
    let spoken: Vec<&str> = home
        .iter()
        .map(|row| match row {
            MenuRow::Fact { spoken, .. } => spoken.as_str(),
            _ => "",
        })
        .collect();
    assert_eq!(
        spoken,
        ["Home: m15, set on the accounts sheet since 2 Oct", ""]
    );
    let budget = rows
        .iter()
        .find_map(|row| match row {
            MenuRow::Fact { label, spoken, .. } if label == "Budget" => Some(spoken.as_str()),
            _ => None,
        })
        .expect("the Budget verdict");
    assert_eq!(budget, "Budget: 84 percent left, resets Saturday 10:00");
}
