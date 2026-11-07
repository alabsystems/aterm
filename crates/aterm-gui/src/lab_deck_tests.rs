// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::accounts::{Cli, Vendor};
use aterm_agent::harness::alab_roster::LabAccount;
use aterm_agent::harness::alab_seen::{LastUse, SignedIn, WindowSeen};

const NOW: i64 = 1_800_000_000;

fn account(pk: &str, label: &str, preferred: &[(&str, Option<u64>)]) -> LabAccount {
    LabAccount {
        pk: pk.into(),
        cli: Cli::Claude,
        vendor: Vendor::default(),
        label: label.into(),
        legacy_label: None,
        email: format!("{label}@example.test"),
        identity: Some(format!("id-{label}@example.test")),
        browser: None,
        joinable: true,
        preferred: preferred
            .iter()
            .map(|(m, rank)| ((*m).to_string(), *rank))
            .collect(),
    }
}

fn window(name: &str, used: u16, read_ago: i64) -> WindowSeen {
    WindowSeen {
        window: name.into(),
        sub: None,
        figure: Some(Figure::Used {
            pct_x10: used,
            reset: Some(NOW + 3_600),
        }),
        read: Some((NOW - read_ago, "m3".into(), "statusline".into())),
        wall_until: None,
    }
}

fn used_by(by: &str, ago: i64) -> AccountSeen {
    AccountSeen {
        last_use: Some(LastUse {
            by: by.into(),
            t: NOW - ago,
        }),
        ..AccountSeen::default()
    }
}

fn fact<'a>(row: &'a DeckAccount, name: &str) -> &'a str {
    &row.facts
        .iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("no {name} in {:?}", row.facts))
        .1
}

fn every_word(deck: &LabDeck) -> String {
    let mut out = String::new();
    for row in &deck.rows {
        out.push_str(&row.label);
        out.push_str(&row.summary);
        for (n, v) in deck.details(row.id) {
            out.push_str(&n);
            out.push_str(&v);
        }
    }
    out
}

/// Negative control: `label_of` returning `account.label` as it stands puts the mistyped label's
/// `@` on the glass and fails the first assertion; reading `email` or `identity` for a row's name
/// fails it for every row.
#[test]
fn rows_name_an_account_by_its_label_never_an_address() {
    let lab = Lab {
        me: Some("m1".into()),
        accounts: vec![
            account(
                "ac-0a1b2c3d4e5f60718293a4",
                "claude-0a1b2c",
                &[("m1", Some(1))],
            ),
            account(
                "ac-9f8e7d6c5b4a39281706f5",
                "typed@example.test",
                &[("m1", Some(2))],
            ),
        ],
        ..Lab::default()
    };
    let deck = project(Some(&lab), None, NOW);
    let words = every_word(&deck);
    assert!(!words.contains('@'), "{words}");
    assert_eq!(deck.rows[0].label, "claude-0a1b2c");
    assert_eq!(
        deck.rows[1].label, "claude-9f8e7d",
        "§0's default label in its place"
    );
    // The fixture the dev seam shows holds no address either.
    assert!(!every_word(&fixture(NOW)).contains('@'));
}

/// Negative controls: sorting this Mac's rows by label alone puts `b-first` (rank 2) before
/// `z-first` (rank 1); dropping the journal half leaves `elsewhere` out; keeping accounts the
/// journal says nothing of adds `silent`.
#[test]
fn this_macs_preferred_accounts_come_first_and_a_preference_limits_nothing() {
    let lab = Lab {
        me: Some("m1".into()),
        accounts: vec![
            account("ac-01", "b-first", &[("m1", Some(2))]),
            account("ac-02", "z-first", &[("m1", Some(1))]),
            account("ac-03", "elsewhere", &[("m2", Some(1))]),
            account("ac-04", "older", &[]),
            account("ac-05", "silent", &[("m2", None)]),
        ],
        ..Lab::default()
    };
    let mut seen = Seen {
        at: NOW,
        ..Seen::default()
    };
    seen.accounts.insert("ac-03".into(), used_by("m1", 60));
    seen.accounts.insert("ac-04".into(), used_by("m4", 86_400));
    seen.accounts.insert("ac-05".into(), AccountSeen::default());
    let deck = project(Some(&lab), Some(&seen), NOW);
    let labels: Vec<_> = deck.rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(labels, ["z-first", "b-first", "elsewhere", "older"]);
    assert!(deck.rows[0].here && deck.rows[1].here && !deck.rows[2].here);
    assert_eq!(
        fact(&deck.rows[2], "Last used by"),
        "m1, about 1 min ago (its clock)"
    );
    assert_eq!(fact(&deck.rows[2], "Preferred by"), "m2");
    // No lab, no rows: the band then draws exactly what it drew before.
    assert!(project(None, Some(&seen), NOW).is_empty());
}

/// Negative controls: taking the first window rather than the most used reads 5h's 62% left;
/// skipping the wall leaves the cell a heat; letting a workspace's window bind makes it 1% left.
/// Reusing the sign-in words or projection clock leaves their ages unchanged or labels the
/// future record as one made in the past.
#[test]
fn quota_left_is_the_binding_windows_with_its_readings_age() {
    let lab = Lab {
        me: Some("m1".into()),
        accounts: vec![
            account("ac-01", "two-windows", &[("m1", Some(1))]),
            account("ac-02", "walled", &[("m1", Some(2))]),
            account("ac-03", "unread", &[("m1", Some(3))]),
        ],
        ..Lab::default()
    };
    let mut seen = Seen {
        at: NOW,
        ..Seen::default()
    };
    let mut workspace = window("7d", 990, 30);
    workspace.sub = Some("sa-01".into());
    seen.accounts.insert(
        "ac-01".into(),
        AccountSeen {
            windows: vec![window("5h", 380, 720), window("7d", 615, 1_800), workspace],
            signed_in: vec![
                SignedIn {
                    on: "m3".into(),
                    state: "in".into(),
                    t: NOW - 60,
                },
                SignedIn {
                    on: "m4".into(),
                    state: "out".into(),
                    t: NOW + 60,
                },
            ],
            ..AccountSeen::default()
        },
    );
    let mut wall = window("5h", 1000, 120);
    wall.read = Some((NOW + 120, "m3".into(), "statusline".into()));
    wall.wall_until = Some(Some(NOW + 3_600));
    seen.accounts.insert(
        "ac-02".into(),
        AccountSeen {
            last_use: Some(LastUse {
                by: "m3".into(),
                t: NOW + 300,
            }),
            windows: vec![window("7d", 200, 60), wall],
            ..AccountSeen::default()
        },
    );
    let mut unread = window("5h", 0, 60);
    unread.figure = Some(Figure::ResetUnread { reset: NOW - 60 });
    seen.accounts.insert(
        "ac-03".into(),
        AccountSeen {
            windows: vec![unread],
            ..AccountSeen::default()
        },
    );
    let original = seen.clone();
    let deck = project(Some(&lab), Some(&seen), NOW);
    let two = &deck.rows[0];
    assert_eq!(two.cell, Cell::Used(615));
    assert_eq!(
        fact(two, "Quota left"),
        "38.5% of 7d left, read on m3, about 30 min ago (its clock) (statusline)"
    );
    let sign_ins = "m3 (in), about 1 min ago (its clock); m4 (out), 1 min ahead (its clock)";
    assert_eq!(fact(two, "Sign-in records"), sign_ins);
    assert!(
        two.summary
            .contains(&format!("sign-in records: {sign_ins}"))
    );
    assert!(two.summary.contains("38.5% of 7d left"), "{}", two.summary);
    let walled = &deck.rows[1];
    assert_eq!(walled.cell, Cell::AtLimit);
    assert!(fact(walled, "Quota left").starts_with("5h at its limit until "));
    assert!(
        fact(walled, "Quota left").contains("read on m3, 2 min ahead (its clock) (statusline)")
    );
    assert_eq!(fact(walled, "Last used by"), "m3, 5 min ahead (its clock)");
    assert!(
        walled
            .summary
            .contains("last used by m3, 5 min ahead (its clock)")
    );
    let unread = &deck.rows[2];
    assert_eq!(unread.cell, Cell::Unknown, "absent, never zero");
    assert!(fact(unread, "Quota left").starts_with("Unknown"));
    let later = project(Some(&lab), Some(&seen), NOW + 3_600);
    let sign_ins = "m3 (in), about 1 h ago (its clock); m4 (out), about 59 min ago (its clock)";
    assert_eq!(fact(&later.rows[0], "Sign-in records"), sign_ins);
    assert!(
        later.rows[0]
            .summary
            .contains(&format!("sign-in records: {sign_ins}"))
    );
    assert!(
        fact(&later.rows[1], "5h window")
            .contains("read on m3, about 58 min ago (its clock) (statusline)")
    );
    assert_eq!(
        fact(&later.rows[1], "Last used by"),
        "m3, about 55 min ago (its clock)"
    );
    assert!(
        later.rows[1]
            .summary
            .contains("last used by m3, about 55 min ago (its clock)")
    );
    assert_eq!(seen, original);
}

/// Negative control: a `latest` that locks rather than tries waits on the held guard and hangs
/// this test; one that hands back an unchanged deck makes the second ask `Some`.
#[test]
fn the_feed_answers_at_once_and_only_with_a_change() {
    let mut feed = Feed::default();
    let shown = Arc::new(LabDeck::default());
    assert!(feed.latest(&shown).is_none(), "nothing projected yet");
    let deck = fixture(NOW);
    feed.hold(deck.clone());
    {
        let _busy = feed.held.lock().expect("feed");
        let held = Arc::clone(&feed.held);
        let mut other = Feed {
            held,
            started: true,
            ..Feed::default()
        };
        assert!(
            other.latest(&shown).is_none(),
            "a held lock answers nothing"
        );
    }
    let got = feed.latest(&shown).expect("the new rows");
    assert_eq!(got.as_ref(), &deck);
    assert_eq!(
        feed.revision(),
        1,
        "a change moves the revision the frame key hashes"
    );
    assert!(feed.latest(&got).is_none(), "no change, no answer");
    assert_eq!(feed.revision(), 1);
}

fn band(deck: &LabDeck, w: f32) -> (Vec<crate::widget::DrawPrim>, Vec<crate::stats_paint::Hit>) {
    let g = crate::stats_deck::DeckGeom {
        w,
        h: 200.0,
        pad: 0.0,
        scale: 2.0,
        font_px: 14.0,
    };
    let inks = crate::stats_deck::cached_inks([20, 20, 20], [230, 230, 230], [100, 180, 200]);
    let snapshot = aterm_types::network_state::NetworkStateSnapshot::default();
    crate::network_deck::paint_with_inks(&g, 340.0, &snapshot, deck, None, None, &inks, false)
}

fn lab_hits(hits: &[crate::stats_paint::Hit]) -> Vec<u64> {
    hits.iter()
        .filter_map(|hit| match hit.kind {
            crate::stats_paint::HitKind::Network(crate::network_deck::Target::LabAccount(id)) => {
                Some(id)
            }
            _ => None,
        })
        .collect()
}

/// Negative controls: drawing a cell with no rows gives the empty band a lab hit; a fit that
/// ignores the `+N` it must leave room for draws a cell under it; filling an
/// unknown cell, or a wall with the heat, fails the ink checks; a cell that grows the band past
/// its fixed 30 pt fails the bounds check.
#[test]
fn the_shared_state_line_draws_one_cell_per_account_within_the_band() {
    use crate::widget::DrawPrim;
    let before = band(&LabDeck::default(), 2_800.0);
    assert_eq!(lab_hits(&before.1), Vec::<u64>::new());
    let deck = fixture(NOW);
    let (prims, hits) = band(&deck, 2_800.0);
    let ids: Vec<u64> = deck.rows.iter().map(|row| row.id).collect();
    let mut shown = lab_hits(&hits);
    shown.reverse();
    assert_eq!(shown, ids, "every row a cell, in row order");
    assert!(
        prims
            .iter()
            .any(|p| matches!(p, DrawPrim::Text { s, .. } if s == "Lab accounts"))
    );
    let top = 340.0;
    let foot = top + crate::network_deck::height_px(2.0) as f32;
    for hit in &hits {
        let (_, y, _, h) = hit.rect;
        assert!(y >= top && y + h <= foot, "{hit:?} inside the band");
    }
    let inks = crate::stats_deck::cached_inks([20, 20, 20], [230, 230, 230], [100, 180, 200]);
    let fills: Vec<[u8; 4]> = prims
        .iter()
        .filter_map(|p| match p {
            DrawPrim::Panel { fill, w, .. } if (*w - 16.0).abs() < 0.01 => Some(*fill),
            _ => None,
        })
        .collect();
    let heat = inks.heat[usize::from(aterm_vitals::ink::heat_step(0.38))];
    assert_eq!(
        fills,
        [
            crate::widget::rgba(heat, 255),
            crate::widget::rgba(inks.over, 255)
        ],
        "a used window's heat, a wall's over-max ink, and no fill for no reading"
    );
    // Narrow: as many cells as fit beside a `+N` that counts the rest.
    let (prims, hits, fit) = (400..1_400)
        .rev()
        .step_by(4)
        .map(|w| {
            let (prims, hits) = band(&deck, w as f32);
            let fit = lab_hits(&hits).len();
            (prims, hits, fit)
        })
        .find(|(_, _, fit)| (1..deck.rows.len()).contains(fit))
        .expect("a width that fits some cells and not all");
    let more = format!("+{}", deck.rows.len() - fit);
    let plus = prims
        .iter()
        .find_map(|p| match p {
            DrawPrim::Text { s, x, .. } if *s == more => Some(*x),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{more} drawn"));
    assert!(hits.iter().all(|h| match h.kind {
        crate::stats_paint::HitKind::Network(crate::network_deck::Target::LabAccount(_)) =>
            h.rect.0 + h.rect.2 <= plus + 6.0,
        _ => true,
    }));
}

/// Negative control: a `targets_with` that leaves the lab out never selects an account, so ↓
/// from Secrets wraps to the overview.
#[test]
fn the_lens_steps_from_secrets_onto_the_accounts_and_shows_their_rows() {
    use crate::network_deck::{Target, details_lab, step};
    let snapshot = aterm_types::network_state::NetworkStateSnapshot::default();
    let deck = fixture(NOW);
    assert!(
        fact(&deck.rows[1], "Quota left")
            .contains("read on m2, 2 min ahead (its clock) (statusline)")
    );
    assert_eq!(
        fact(&deck.rows[1], "Last used by"),
        "m2, 5 min ahead (its clock)"
    );
    let first = Target::LabAccount(deck.rows[0].id);
    assert_eq!(
        step(&snapshot, &deck, None, (700.0, 1.0), Target::Secrets, true),
        first
    );
    let rows = details_lab(&snapshot, &deck, None, first);
    assert_eq!(rows[0].0, "Account");
    assert!(rows[0].1.starts_with("claude-0a1b2c"), "{rows:?}");
    for other in &deck.rows[1..] {
        assert!(
            rows.iter().any(|(name, _)| *name == other.label),
            "{rows:?}"
        );
    }
    let overview = details_lab(&snapshot, &deck, None, Target::Overview);
    assert!(overview.iter().any(|(name, _)| name == "Lab accounts"));
    let bare = details_lab(&snapshot, &LabDeck::default(), None, Target::Overview);
    assert!(!bare.iter().any(|(name, _)| name == "Lab accounts"));
}

/// Negative control: a `project` that reads an unread journal as an empty one says `No use in
/// the lab journal` and fails the first assertion; one that drops the `read_at` check in
/// `overview` says the journal was read.
#[test]
fn an_unread_journal_is_not_read_as_an_empty_one() {
    let lab = Lab {
        me: Some("m1".into()),
        accounts: vec![account("ac-01", "mine", &[("m1", Some(1))])],
        ..Lab::default()
    };
    let unread = project(Some(&lab), None, NOW);
    let row = &unread.rows[0];
    assert_eq!(fact(row, "Last used by"), "The lab journal is not read yet");
    assert_eq!(
        fact(row, "Quota left"),
        "Unknown; the lab journal is not read yet"
    );
    assert_eq!(
        fact(row, "Sign-in records"),
        "The lab journal is not read yet"
    );
    let (_, line) = unread.overview().expect("a row");
    assert!(line.contains("not read yet"), "{line}");
    let read = project(
        Some(&lab),
        Some(&Seen {
            at: NOW,
            ..Seen::default()
        }),
        NOW,
    );
    assert_eq!(
        fact(&read.rows[0], "Last used by"),
        "No use in the lab journal"
    );
    assert_eq!(
        fact(&read.rows[0], "Sign-in records"),
        "No sign-in record in the lab journal"
    );
    assert!(!read.overview().expect("a row").1.contains("not read yet"));
}

/// Negative control: dropping either pass's `joinable` filter gives the retired or the paused
/// account a cell; dropping `held` from the overview leaves them uncounted.
#[test]
fn an_account_held_from_use_has_no_cell_and_is_counted() {
    let mut retired = account("ac-01", "retired", &[("m1", Some(1))]);
    retired.joinable = false;
    let mut paused = account("ac-02", "paused", &[]);
    paused.joinable = false;
    let mut unseen = account("ac-03", "quiet", &[]);
    unseen.joinable = false;
    let lab = Lab {
        me: Some("m1".into()),
        accounts: vec![
            retired,
            paused,
            unseen,
            account("ac-04", "live", &[("m1", Some(2))]),
        ],
        ..Lab::default()
    };
    let mut seen = Seen {
        at: NOW,
        ..Seen::default()
    };
    seen.accounts.insert("ac-02".into(), used_by("m2", 60));
    let deck = project(Some(&lab), Some(&seen), NOW);
    let labels: Vec<_> = deck.rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(labels, ["live"]);
    assert_eq!(deck.held, 2, "the ones that would have had a cell");
    let (_, line) = deck.overview().expect("a row");
    assert!(
        line.contains("2 paused, retired or barred not shown"),
        "{line}"
    );
}

/// Negative control: an `as_of` that keeps the passed wall draws `AtLimit` and says `at its
/// limit until` a time already gone; one that keeps a figure past its reset draws `Used(900)`.
#[test]
fn a_reading_older_than_its_wall_or_reset_draws_neither() {
    let lab = Lab {
        me: Some("m1".into()),
        accounts: vec![
            account("ac-01", "lifted", &[("m1", Some(1))]),
            account("ac-02", "reset", &[("m1", Some(2))]),
            account("ac-03", "standing", &[("m1", Some(3))]),
        ],
        ..Lab::default()
    };
    let mut seen = Seen {
        at: NOW - 180,
        ..Seen::default()
    };
    let mut lifted = window("5h", 1000, 300);
    lifted.figure = Some(Figure::Used {
        pct_x10: 1000,
        reset: Some(NOW - 60),
    });
    lifted.wall_until = Some(Some(NOW - 60));
    seen.accounts.insert(
        "ac-01".into(),
        AccountSeen {
            windows: vec![lifted],
            ..AccountSeen::default()
        },
    );
    let mut reset = window("5h", 900, 300);
    reset.figure = Some(Figure::Used {
        pct_x10: 900,
        reset: Some(NOW - 1),
    });
    seen.accounts.insert(
        "ac-02".into(),
        AccountSeen {
            windows: vec![reset, window("7d", 200, 300)],
            ..AccountSeen::default()
        },
    );
    let mut standing = window("5h", 1000, 300);
    standing.wall_until = Some(Some(NOW + 60));
    seen.accounts.insert(
        "ac-03".into(),
        AccountSeen {
            windows: vec![standing],
            ..AccountSeen::default()
        },
    );
    let deck = project(Some(&lab), Some(&seen), NOW);
    assert_eq!(
        deck.rows[0].cell,
        Cell::Unknown,
        "the wall lifted, the window reset"
    );
    assert!(!fact(&deck.rows[0], "Quota left").contains("at its limit"));
    assert!(fact(&deck.rows[0], "5h window").contains("not read since"));
    assert_eq!(
        deck.rows[1].cell,
        Cell::Used(200),
        "the 7d binds once 5h reset"
    );
    assert_eq!(
        deck.rows[2].cell,
        Cell::AtLimit,
        "a wall still standing stands"
    );
}
