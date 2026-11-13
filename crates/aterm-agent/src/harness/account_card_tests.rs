// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`build`] over hand-made inputs: who the card is about for each kind of tab, and that each
//! thing it cannot know (no pool file, no claims table, an unreadable table, no journal, an
//! outside login's end) is its own value and never an empty answer.

use super::*;
use crate::harness::account_history::HistoryRead;
use crate::harness::account_series::{Fig, Said, SeriesRow};
use crate::harness::alab_seen::{LastUse, Line, SESSION_START, Seen as JournalSeen, WindowSeen};
use crate::harness::claims::{Book, Claim, Kind, View};

const H: i64 = 3600;
const D: i64 = 86_400;
/// Wed 11 Nov 2026 18:20 UTC.
const NOW: i64 = 1_794_421_200;
const ME: &str = "aa11";
const PEER: &str = "bb22";

fn utc(_: i64) -> i64 {
    0
}

fn row(label: &str, email: &str) -> Row {
    Row {
        label: label.into(),
        email: Some(email.into()),
        ..Row::default()
    }
}

fn roster() -> Roster {
    Roster {
        rows: vec![
            row("alpha", "alpha.m20.1@example.test"),
            row("beta", "beta.m21.1@example.test"),
        ],
    }
}

fn state() -> State {
    State {
        machine: "machine0".into(),
        swarm_label: Some("m20".into()),
        ..State::default()
    }
}

/// What a worker read, owned, so each test changes one thing.
struct Read {
    cli: Cli,
    roster: Roster,
    state: State,
    enrolled: BTreeSet<String>,
    label: Option<&'static str>,
    email: Option<&'static str>,
    pool: Option<Pool>,
    claims: Result<Option<(View, Book)>, String>,
    journal: Journal,
}

impl Read {
    fn new() -> Read {
        Read {
            cli: Cli::Claude,
            roster: roster(),
            state: state(),
            enrolled: ["alpha".to_owned()].into(),
            label: Some("alpha"),
            email: Some("alpha.m20.1@example.test"),
            pool: None,
            claims: Ok(None),
            journal: Journal::Off,
        }
    }

    fn outside() -> Read {
        Read {
            label: None,
            email: Some("unicorn.m15.2@example.test"),
            ..Read::new()
        }
    }

    fn card_with(&self, history: HistoryIn<'_>) -> AccountCard {
        build(&Inputs {
            cli: self.cli,
            roster: &self.roster,
            state: &self.state,
            enrolled: &self.enrolled,
            logins: &Logins::default(),
            session: Session {
                label: self.label,
                email: self.email,
                profile_key: None,
            },
            pool: self.pool.as_ref(),
            claims: &self.claims,
            journal: &self.journal,
            history,
            now: NOW,
            tz_at: utc,
        })
    }

    fn card(&self) -> AccountCard {
        self.card_with(HistoryIn::NotRead)
    }
}

fn pool(text: &str) -> Pool {
    Pool::from_toml(&format!("version = 1\n{text}"), "ai-pool.toml").expect("a pool file")
}

fn claim(email: &str, since: i64) -> Claim {
    Claim {
        key: LeaseKey::for_cli(Cli::Claude, None, email).expect("a key"),
        kind: Kind::Lease,
        since,
    }
}

fn claims_row(machine: &str, label: &str, claims: Vec<Claim>) -> claims::Row {
    claims::Row {
        machine: machine.into(),
        label: label.into(),
        counter: 1,
        until: NOW + H,
        claims,
    }
}

fn table(rows: Vec<claims::Row>) -> Result<Option<(View, Book)>, String> {
    Ok(Some((
        View {
            me: ME.into(),
            ..View::default()
        },
        Book::of(rows),
    )))
}

fn mac(name: &str) -> Mac {
    Mac {
        name: name.into(),
        ours: false,
        named: true,
    }
}

fn this_mac() -> Mac {
    Mac {
        name: "m20".into(),
        ours: true,
        named: true,
    }
}

/// The lab key a test's journal files an address under.
fn keyer(_realm: &str, email: &str) -> Option<String> {
    Some(match email {
        "alpha.m20.1@example.test" => "ac-00000000000000a1".to_owned(),
        "unicorn.m15.2@example.test" => "ac-00000000000000c3".to_owned(),
        _ => return None,
    })
}

fn journal_of(key: &str, account: AccountSeen) -> Journal {
    Journal::Read(Arc::new(JournalSeen {
        accounts: [(key.to_owned(), account)].into(),
        at: NOW,
    }))
}

/// WHO THE CARD IS ABOUT. A Claude Code tab on a roster row is that row, by its email, with
/// whether its login was ever made here; on a login no row lists it is that address and no row
/// (never the store's active row); with nothing named it is unknown. Another pooled CLI's tab
/// is the store's active row. Negative control: the label of a row whose address contradicts
/// the session's is not taken.
#[test]
fn who_is_the_sessions_row_an_outside_address_the_active_row_or_unknown() {
    let mut read = Read::new();
    read.state.active = Some("beta".into());
    assert_eq!(
        read.card().who,
        Who::Pool {
            label: "alpha".into(),
            name: "alpha.m20.1@example.test".into(),
            enrolled: true,
        }
    );
    read.label = Some("beta");
    read.email = Some("beta.m21.1@example.test");
    assert_eq!(
        read.card().who,
        Who::Pool {
            label: "beta".into(),
            name: "beta.m21.1@example.test".into(),
            enrolled: false,
        }
    );
    // No label: the row is found by the session's address.
    read.label = None;
    assert_eq!(read.card().who.label(), Some("beta"));
    // A label whose row names another address: the session is not that row.
    read.label = Some("alpha");
    read.email = Some("unicorn.m15.2@example.test");
    assert_eq!(
        read.card().who,
        Who::Outside {
            email: "unicorn.m15.2@example.test".into()
        },
        "CONTROL: a contradicted label is not taken, nor the active row"
    );
    // A label whose row names another address the roster lists (a footer still naming the
    // old row after a /login): the session is the row its address names, never outside.
    read.email = Some("beta.m21.1@example.test");
    assert_eq!(
        read.card().who,
        Who::Pool {
            label: "beta".into(),
            name: "beta.m21.1@example.test".into(),
            enrolled: false,
        },
        "the address's row, not an outside login"
    );
    read.label = None;
    read.email = None;
    assert_eq!(read.card().who, Who::Unknown);
    read.cli = Cli::Codex;
    assert_eq!(
        read.card().who,
        Who::Active {
            label: "beta".into(),
            name: "beta.m21.1@example.test".into(),
        }
    );
    read.state.active = None;
    assert_eq!(read.card().who, Who::Unknown);
}

/// THE CARD NAMES ROWS BY THEIR SHOWN NAMES. Every roster label has its email in `names`, the
/// pool's size is the roster's, this Mac's lab label is carried, and a pinned Mac names the
/// pinned account by its email. Negative control: unpinned, there is no pinned name.
#[test]
fn the_card_carries_names_the_pool_size_this_mac_and_the_pin() {
    let mut read = Read::new();
    read.state.active = Some("beta".into());
    let card = read.card();
    assert_eq!(card.pool_size, 2);
    assert_eq!(card.this_mac.as_deref(), Some("m20"));
    assert_eq!(
        card.names.get("beta").map(String::as_str),
        Some("beta.m21.1@example.test")
    );
    assert_eq!(card.pinned, None, "CONTROL: active is not pinned");
    assert_eq!(card.read_at, NOW);
    read.state.pinned = true;
    assert_eq!(
        read.card().pinned.as_deref(),
        Some("beta.m21.1@example.test")
    );
}

/// AN OUTSIDE LOGIN IS NEVER GIVEN AN END. A roster row whose login was made here has its
/// stored login's half; a row never signed in here has none; and an outside login has none
/// either, whatever the records hold, because nothing here knows when it ends.
#[test]
fn an_outside_login_is_never_given_an_end() {
    let read = Read::new();
    assert_eq!(read.card().login, Some(LoginHalf::Ok));
    let mut unenrolled = Read::new();
    unenrolled.enrolled.clear();
    assert_eq!(unenrolled.card().login, None);
    assert_eq!(Read::outside().card().login, None);
}

/// THE HOME IS THE POOL FILE'S, AND ITS ABSENCE IS SAID. With no pool file the home is not
/// known; with a file that does not list the address there is no home on record; listed, the
/// home is `warm[0]` with its reason, the date its slot began, the other Macs it is assigned
/// to and the home slot's last-used day. A `since` that is no date leaves the date out and
/// keeps the home. An outside login is found by its address alone.
#[test]
fn the_home_comes_from_the_pool_file_or_says_why_not() {
    let mut read = Read::new();
    assert_eq!(read.card().machine.home, Home::NoPoolFile);
    read.pool = Some(pool(
        "[[account]]\nprovider = \"claude\"\nemail = \"other@example.test\"\nwarm = [\"m3\"]\n",
    ));
    assert_eq!(read.card().machine.home, Home::NotListed);
    read.pool = Some(pool(
        "[[account]]\nprovider = \"claude\"\nemail = \"alpha.m20.1@example.test\"\nwarm = []\n",
    ));
    assert_eq!(
        read.card().machine.home,
        Home::NotListed,
        "listed with no Mac: no home on record"
    );
    read.pool = Some(pool(
        "[[account]]\nprovider = \"claude\"\nemail = \"Alpha.m20.1@example.test\"\n\
         warm = [\"m22\", \"m20\"]\nsince = [\"2026-10-28T04:12:00Z\", \"2026-11-01\"]\n\
         why = [\"pin\", \"fill\"]\nused = [\"2026-11-09\", \"2026-11-11\"]\n",
    ));
    let card = read.card();
    assert_eq!(
        card.machine.home,
        Home::Known {
            mac: mac("m22"),
            why: Some(SlotWhy::Pin),
            since: aterm_types::rfc3339::parse_utc("2026-10-28T04:12:00Z"),
            also: vec![this_mac()],
        }
    );
    assert_eq!(
        card.machine.used_day,
        aterm_types::rfc3339::parse_utc("2026-11-09T12:00:00Z"),
        "a date alone is its noon"
    );
    read.pool = Some(pool(
        "[[account]]\nprovider = \"claude\"\nemail = \"alpha.m20.1@example.test\"\n\
         warm = [\"m20\"]\nsince = [\"last tuesday\"]\nwhy = [\"fill\"]\nused = [\"\"]\n",
    ));
    let card = read.card();
    assert_eq!(
        card.machine.home,
        Home::Known {
            mac: this_mac(),
            why: Some(SlotWhy::Fill),
            since: None,
            also: Vec::new(),
        }
    );
    assert_eq!(card.machine.used_day, None);
    let mut outside = Read::outside();
    outside.pool = Some(pool(
        "[[account]]\nprovider = \"claude\"\nemail = \"unicorn.m15.2@example.test\"\n\
         warm = [\"m15\"]\nsince = [\"2026-10-02\"]\nwhy = [\"pin\"]\n",
    ));
    assert!(matches!(
        outside.card().machine.home,
        Home::Known { ref mac, why: Some(SlotWhy::Pin), since: Some(_), .. } if mac.name == "m15"
    ));
}

const POOL_ALPHA: &str =
    "[[account]]\nprovider = \"claude\"\nemail = \"alpha.m20.1@example.test\"\nwarm = [\"m20\"]\n";

/// A CLAIM ELSEWHERE IS A HOLDER WITH WHEN IT BEGAN, AND OURS IS OURS. The table's live claims
/// on the account are listed, earliest first: another Mac by its label, this Mac (the table's
/// own machine) as ours, and a Mac with no label as unnamed. A lapsed row and a claim on another
/// account are not listed. An outside login's claims are found by its address, with no roster
/// row.
#[test]
fn live_claims_are_holders_ours_and_others_by_when_they_began() {
    let mut read = Read::new();
    read.pool = Some(pool(POOL_ALPHA));
    let alpha = "alpha.m20.1@example.test";
    let mut lapsed = claims_row("cc33", "m30", vec![claim(alpha, NOW - 9 * H)]);
    lapsed.until = NOW - 1;
    read.claims = table(vec![
        claims_row(ME, "m20", vec![claim(alpha, NOW - 2 * H)]),
        claims_row(PEER, "m22", vec![claim(alpha, NOW - 5 * H)]),
        claims_row("dd44", "", vec![claim(alpha, NOW - 3 * H)]),
        claims_row(
            "ee55",
            "m31",
            vec![claim("beta.m21.1@example.test", NOW - 8 * H)],
        ),
        lapsed,
    ]);
    assert_eq!(
        read.card().machine.claims,
        Claims::Read(vec![
            Holder {
                mac: mac("m22"),
                since: Some(NOW - 5 * H),
                until: NOW + H,
            },
            Holder {
                mac: Mac {
                    name: String::new(),
                    ours: false,
                    named: false,
                },
                since: Some(NOW - 3 * H),
                until: NOW + H,
            },
            Holder {
                mac: this_mac(),
                since: Some(NOW - 2 * H),
                until: NOW + H,
            },
        ])
    );
    let mut outside = Read::outside();
    outside.pool = Some(pool(POOL_ALPHA));
    outside.claims = table(vec![claims_row(
        PEER,
        "m15",
        vec![claim("unicorn.m15.2@example.test", NOW - 5 * H)],
    )]);
    assert_eq!(
        outside.card().machine.claims,
        Claims::Read(vec![Holder {
            mac: mac("m15"),
            since: Some(NOW - 5 * H),
            until: NOW + H,
        }])
    );
    // A table read with nobody on the account: nobody holds it.
    outside.claims = table(Vec::new());
    assert_eq!(outside.card().machine.claims, Claims::Read(Vec::new()));
}

/// NO CLAIMS TABLE IS NOT KNOWN, NEVER NOT IN USE. With no table in this process the claims are
/// absent; a table that could not be read is unreadable; a program whose accounts are not in
/// the claims ring is not tracked; and an empty table on a Mac with no pool file (where no
/// claim is made) is absent too. Where this Mac's state holds the senior claim it last
/// settled, still live, that one holder is given as the senior and no list is claimed; a
/// lapsed one is not.
#[test]
fn no_claims_table_says_not_known_never_not_in_use() {
    let mut read = Read::new();
    read.pool = Some(pool(POOL_ALPHA));
    assert_eq!(read.card().machine.claims, Claims::Absent);
    read.claims = Err("the roster is busy".into());
    assert_eq!(read.card().machine.claims, Claims::Unreadable);
    read.state.leases.insert(
        "alpha".into(),
        accounts::AccountLease {
            holder: "m22".into(),
            until: NOW + H,
            at: NOW - 4 * H,
        },
    );
    let senior = |why| {
        Claims::Senior(
            Holder {
                mac: mac("m22"),
                since: Some(NOW - 4 * H),
                until: NOW + H,
            },
            why,
        )
    };
    assert_eq!(
        read.card().machine.claims,
        senior(NoTable::Unreadable),
        "the senior holder keeps why no table spoke"
    );
    read.claims = Ok(None);
    assert_eq!(read.card().machine.claims, senior(NoTable::Absent));
    read.state
        .leases
        .get_mut("alpha")
        .expect("the lease")
        .holder = "claim:ffee".into();
    assert!(matches!(
        read.card().machine.claims,
        Claims::Senior(Holder { ref mac, .. }, NoTable::Absent) if !mac.named && !mac.ours
    ));
    read.state.leases.get_mut("alpha").expect("the lease").until = NOW;
    assert_eq!(read.card().machine.claims, Claims::Absent, "a lapsed lease");
    // An empty table where no pool file is: claims are not made here, so it proves nothing.
    read.pool = None;
    read.claims = table(Vec::new());
    assert_eq!(read.card().machine.claims, Claims::Absent);
    read.pool = Some(pool(POOL_ALPHA));
    assert_eq!(
        read.card().machine.claims,
        Claims::Read(Vec::new()),
        "CONTROL: with a pool file an empty table is nobody"
    );
    for cli in [Cli::Grok, Cli::Kimi] {
        read.cli = cli;
        read.state.active = Some("alpha".into());
        assert_eq!(read.card().machine.claims, Claims::NotTracked, "{cli:?}");
    }
    read.cli = Cli::Codex;
    assert_ne!(read.card().machine.claims, Claims::NotTracked);
}

/// A HOLD FOR A MAC WHOSE CLOCK DISAGREES IS NOT A CLAIM IT MADE. The hold this Mac keeps on a
/// home account of a skewed peer ([`claims::holds`]) reaches the card as a holder with no
/// beginning, listed after every real claim, so nothing formats its sentinel as a time. Where
/// the peer's own row is live too, that row's beginning is the holder's. Negative control: a
/// hold that has ended is no holder.
#[test]
fn a_hold_for_a_skewed_mac_has_no_beginning() {
    let alpha = "alpha.m20.1@example.test";
    let key = LeaseKey::for_cli(Cli::Claude, None, alpha).expect("a key");
    let held = |until: i64| {
        claims::holds(
            &[claims::SkewedPeer {
                machine: PEER.into(),
                labels: vec!["m22".into()],
                until,
                ahead_s: 0,
            }],
            &[(key.clone(), "m22".to_owned())],
        )
    };
    let view = |until: i64| View {
        me: ME.into(),
        held: held(until),
        ..View::default()
    };
    let mut read = Read::new();
    read.pool = Some(pool(POOL_ALPHA));
    read.claims = Ok(Some((
        view(NOW + D),
        Book::of(vec![claims_row(ME, "m20", vec![claim(alpha, NOW - 2 * H)])]),
    )));
    assert_eq!(
        read.card().machine.claims,
        Claims::Read(vec![
            Holder {
                mac: this_mac(),
                since: Some(NOW - 2 * H),
                until: NOW + H,
            },
            Holder {
                mac: mac("m22"),
                since: None,
                until: NOW + D,
            },
        ])
    );
    // The peer's own row is live as well: its claim's beginning is known.
    read.claims = Ok(Some((
        view(NOW + D),
        Book::of(vec![claims_row(
            PEER,
            "m22",
            vec![claim(alpha, NOW - 5 * H)],
        )]),
    )));
    assert_eq!(
        read.card().machine.claims,
        Claims::Read(vec![Holder {
            mac: mac("m22"),
            since: Some(NOW - 5 * H),
            until: NOW + D,
        }])
    );
    read.claims = Ok(Some((view(NOW), Book::of(Vec::new()))));
    assert_eq!(
        read.card().machine.claims,
        Claims::Read(Vec::new()),
        "CONTROL: an ended hold"
    );
}

/// AN ACCOUNT NOTHING NAMES WAS NOT LOOKED UP. With no row and no address (and for a row that
/// names no address) the pool file and the claims table are not asked, though both were read:
/// the home, the claims and the history say so, never `not listed`, an empty list of holders or
/// `nothing found`. A row with no address still gives the senior holder this Mac settled, with
/// why no table spoke of the rest. Negative control: the same reads over a named account answer.
#[test]
fn an_account_nothing_names_is_not_asked_never_none() {
    let mut read = Read::new();
    read.pool = Some(pool(POOL_ALPHA));
    read.claims = table(Vec::new());
    let named = read.card();
    assert!(
        matches!(named.machine.home, Home::Known { .. }),
        "CONTROL: a named account's home is found"
    );
    assert_eq!(named.machine.claims, Claims::Read(Vec::new()));
    read.label = None;
    read.email = None;
    let card = read.card();
    assert_eq!(card.who, Who::Unknown);
    assert_eq!(card.machine.home, Home::NotAsked);
    assert_eq!(card.machine.claims, Claims::NotAsked);
    assert_eq!(card.history.from, HistoryFrom::NotRead);
    // Another program's active row with no address: a label, and nothing to ask the table by.
    read.cli = Cli::Codex;
    read.roster.rows[0].email = None;
    read.state.active = Some("alpha".into());
    let card = read.card();
    assert!(matches!(card.who, Who::Active { .. }));
    assert_eq!(card.machine.home, Home::NotAsked);
    assert_eq!(card.machine.claims, Claims::NotAsked);
    read.state.leases.insert(
        "alpha".into(),
        accounts::AccountLease {
            holder: "m22".into(),
            until: NOW + H,
            at: NOW - 4 * H,
        },
    );
    assert!(matches!(
        read.card().machine.claims,
        Claims::Senior(_, NoTable::NotAsked)
    ));
}

/// EVERY INSTANT THE CARD SAYS IS ON ITS OWN OFFSET. The card's clock is sampled over the span
/// its instants lie in, so a zone that changed its offset eleven days ago gives an instant
/// before the change the offset it was said on then, and one a year ahead the offset of then.
/// Negative control: an instant after the change is on today's offset.
#[test]
fn the_cards_clock_spans_a_change_of_offset_weeks_away() {
    // US Pacific in autumn 2026: daylight time ends Sun 1 Nov 09:00 UTC; it begins again on
    // Sun 14 Mar 2027 10:00 UTC.
    const FALL_BACK: i64 = 1_793_523_600;
    const SPRING: i64 = 1_805_018_400;
    fn pacific(t: i64) -> i64 {
        if (FALL_BACK..SPRING).contains(&t) {
            -8 * H
        } else {
            -7 * H
        }
    }
    assert!((10 * D..12 * D).contains(&(NOW - FALL_BACK)));
    let read = Read::new();
    let card = build(&Inputs {
        cli: read.cli,
        roster: &read.roster,
        state: &read.state,
        enrolled: &read.enrolled,
        logins: &Logins::default(),
        session: Session::default(),
        pool: None,
        claims: &read.claims,
        journal: &read.journal,
        history: HistoryIn::NotRead,
        now: NOW,
        tz_at: pacific,
    });
    assert_eq!(card.offsets.at(NOW), -8 * H);
    assert_eq!(
        card.offsets.at(FALL_BACK),
        -8 * H,
        "CONTROL: after the change"
    );
    assert_eq!(card.offsets.at(FALL_BACK - 1), -7 * H);
    assert_eq!(card.offsets.at(NOW - 50 * D), -7 * H);
    assert_eq!(card.offsets.at(SPRING - 1), -8 * H);
    assert_eq!(card.offsets.at(NOW + 300 * D), -7 * H);
}

fn line(t: i64, by: &str, kind: &str) -> Line {
    Line {
        t,
        by: by.into(),
        kind: kind.into(),
        cli: "claude".into(),
        detail: String::new(),
        heard: false,
        window: None,
        sub: false,
        pct_x10: None,
        reset: None,
    }
}

/// A journal line of the account's own week: `pct_x10` used, read by `by` at `t`.
fn week_line(t: i64, by: &str, pct_x10: u16, reset: i64) -> Line {
    Line {
        window: Some(alab_seen::WEEK.into()),
        pct_x10: Some(pct_x10),
        reset: Some(reset),
        ..line(t, by, alab_seen::QUOTA)
    }
}

/// THE JOURNAL IS OFF, NOT READ YET, SILENT OR HELD, AND ONLY HELD GIVES FACTS. Off and
/// not-read-yet are told apart; a journal that says nothing of the account is silent; one that
/// does gives its last use, its starts by Mac with the instant the count starts from, and, for
/// an outside login this Mac's records hold nothing of, its week and five-hour window with the
/// Mac that read them.
#[test]
fn the_journal_is_off_not_read_silent_or_held() {
    crate::harness::journal::with_keyer(keyer, || {
        let mut read = Read::outside();
        assert_eq!(read.card().machine.journal, JournalHeld::Off);
        read.journal = Journal::NotReadYet;
        let card = read.card();
        assert_eq!(card.machine.journal, JournalHeld::NotReadYet);
        assert_eq!(
            (card.machine.last_use, card.machine.starts, card.budget.week),
            (None, None, None)
        );
        read.journal = journal_of("ac-00000000000000a1", AccountSeen::default());
        assert_eq!(
            read.card().machine.journal,
            JournalHeld::Silent,
            "another account's entry"
        );
        let reset = NOW + 2 * D;
        read.journal = journal_of(
            "ac-00000000000000c3",
            AccountSeen {
                last_use: Some(LastUse {
                    by: "m15".into(),
                    t: NOW - 2 * H,
                }),
                signed_in: Vec::new(),
                windows: vec![
                    WindowSeen {
                        window: "7d".into(),
                        sub: None,
                        figure: Some(alab_seen::Figure::Used {
                            pct_x10: 160,
                            reset: Some(reset),
                        }),
                        read: Some((NOW - 60, "m15".into(), "tap".into())),
                        wall_until: None,
                    },
                    WindowSeen {
                        window: "5h".into(),
                        sub: None,
                        figure: Some(alab_seen::Figure::Used {
                            pct_x10: 30,
                            reset: Some(NOW + H),
                        }),
                        read: Some((NOW - 3 * H, "m20".into(), "tap".into())),
                        wall_until: None,
                    },
                    WindowSeen {
                        window: "7d".into(),
                        sub: Some("sa-00000000000000ff".into()),
                        figure: Some(alab_seen::Figure::Used {
                            pct_x10: 990,
                            reset: Some(reset),
                        }),
                        read: Some((NOW - 30, "m15".into(), "tap".into())),
                        wall_until: None,
                    },
                ],
                history: vec![
                    line(NOW - 14 * D, "m3", "quota"),
                    line(NOW - 3 * D, "m15", SESSION_START),
                    line(NOW - 2 * D, "m15", SESSION_START),
                    line(NOW - D, "m20", SESSION_START),
                ],
            },
        );
        let card = read.card();
        assert_eq!(card.machine.journal, JournalHeld::Held);
        assert_eq!(card.machine.last_use, Some((mac("m15"), NOW - 2 * H)));
        assert_eq!(
            card.machine.starts,
            Some((vec![(mac("m15"), 2), (this_mac(), 1)], NOW - 14 * D))
        );
        assert_eq!(
            card.budget.week,
            Some(Seen {
                figure: Figure::Exact(160),
                reset: Some(reset),
                at: Some(NOW - 60),
                by: Some(mac("m15")),
            }),
            "the account's own week, not its workspace's"
        );
        assert_eq!(
            card.budget.five,
            Some(Seen {
                figure: Figure::AtLeast(30),
                reset: Some(NOW + H),
                at: Some(NOW - 3 * H),
                by: None,
            }),
            "an older reading is a bound; read on this Mac names no Mac"
        );
    });
}

fn window(pct_x10: u32, reset: i64, at: i64, by: &str) -> accounts::WindowFig {
    accounts::WindowFig {
        pct_x10,
        reset,
        at,
        by: by.into(),
        source: accounts::SOURCE_TAP.into(),
        epoch_at: None,
    }
}

/// THE BUDGET IS THIS MAC'S RECORDS FIRST: each window's held figure with who read it, a
/// standing wall as out until the reset it named, a per-model week by the model's name, credits,
/// a wall of any other window, and a limit-reset offer that still stands. A figure whose reset
/// passed is no figure. Negative control: a cleared wall leaves the figure.
#[test]
fn the_budget_holds_figures_walls_model_weeks_credits_and_resets() {
    let mut read = Read::new();
    let week_reset = NOW + 2 * D;
    let fable = crate::harness::profile::Bucket::Fable.id();
    {
        let account = read.state.accounts.entry("alpha".into()).or_default();
        account.windows.insert(
            accounts::WEEK_LIMIT.into(),
            window(390, week_reset, NOW - 60, "machine0"),
        );
        account.windows.insert(
            accounts::SESSION_LIMIT.into(),
            window(600, NOW - 1, NOW - 3 * H, "machine0"),
        );
        account
            .windows
            .insert(fable.into(), window(500, week_reset, NOW - 2 * H, "m22"));
        account.credits = Some(accounts::Credits::On(NOW - H));
        account.offer = Some(accounts::Offer {
            kind: None,
            left: 2,
            use_by: Some(NOW + 19 * D),
            at: NOW - H,
            by: "machine0".into(),
        });
        account.walls.insert(
            accounts::MONTH_LIMIT.into(),
            accounts::Wall {
                until: Some(NOW + 5 * D),
                at: NOW - H,
                by: "machine0".into(),
                attempts: 1,
                cleared: false,
            },
        );
    }
    let budget = read.card().budget;
    assert_eq!(
        budget.week,
        Some(Seen {
            figure: Figure::Exact(390),
            reset: Some(week_reset),
            at: Some(NOW - 60),
            by: None,
        })
    );
    assert_eq!(budget.five, None, "its reset passed: not read");
    assert_eq!(
        budget.models,
        [(
            "Fable".to_owned(),
            Seen {
                figure: Figure::AtLeast(500),
                reset: Some(week_reset),
                at: Some(NOW - 2 * H),
                by: Some(mac("m22")),
            }
        )]
    );
    assert_eq!(budget.credits, Some(CreditsWord::On));
    assert_eq!(budget.walls, [("month".to_owned(), Some(NOW + 5 * D))]);
    assert_eq!(budget.resets.as_ref().map(|offer| offer.left), Some(2));
    // A standing week wall is out until its reset, and is not said again as a limit.
    let wall = accounts::Wall {
        until: Some(week_reset),
        at: NOW - 10 * 60,
        by: "m22".into(),
        attempts: 1,
        cleared: false,
    };
    {
        let account = read.state.accounts.get_mut("alpha").expect("the record");
        account
            .walls
            .insert(accounts::WEEK_LIMIT.into(), wall.clone());
        account.offer.as_mut().expect("the offer").use_by = Some(NOW - 1);
        account.credits = Some(accounts::Credits::InUse(accounts::CreditsUse {
            since: NOW - 30 * 60,
            seen: NOW - 60,
            until: Some(week_reset),
            told: false,
        }));
    }
    let budget = read.card().budget;
    assert_eq!(
        budget.week,
        Some(Seen {
            figure: Figure::Out,
            reset: Some(week_reset),
            at: Some(NOW - 10 * 60),
            by: Some(mac("m22")),
        })
    );
    assert_eq!(budget.walls.len(), 1, "the week's wall is the week row's");
    assert_eq!(budget.resets, None, "an offer past its use-by");
    assert_eq!(
        budget.credits,
        Some(CreditsWord::InUse {
            since: NOW - 30 * 60
        })
    );
    read.state
        .accounts
        .get_mut("alpha")
        .expect("the record")
        .walls
        .insert(
            accounts::WEEK_LIMIT.into(),
            accounts::Wall {
                cleared: true,
                ..wall
            },
        );
    assert_eq!(
        read.card().budget.week.map(|seen| seen.figure),
        Some(Figure::Exact(390)),
        "CONTROL: a cleared wall leaves the reading"
    );
    assert_eq!(CreditsWord::of_state("out"), Some(CreditsWord::Out));
    assert_eq!(CreditsWord::of_state("unknown"), None);
}

fn week_reading(at: i64, pct_x10: u32, reset: i64) -> SeriesRow {
    SeriesRow {
        t: at,
        by: "m".into(),
        said: Said::Fig {
            five_hour: None,
            seven_day: Some(Fig {
                pct_x10,
                reset,
                at,
                src: accounts::SOURCE_TAP.into(),
                epoch: None,
            }),
            ac: None,
            key: None,
        },
    }
}

/// HISTORY IS LOOKED FOR BEFORE THE CARD SAYS NONE, AND SAYS WHERE IT IS FROM. A read that
/// found rows gives the series' days (what was left at each day's last reading, a day at 100%
/// used as out) and the eight days of readings, since the oldest row held; a read that found
/// none says nothing was found, and so does one whose rows hold no reading of the week (it
/// keeps their oldest, so the card can tell the two apart); no read made says it was not read,
/// never that there is none; a kept history is carried as it is; and another program's
/// accounts keep none, whatever was read.
#[test]
fn history_is_the_series_read_nothing_found_kept_or_not_kept() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-account-card-history-")
        .tempdir()
        .expect("a scratch directory");
    let reset = NOW + 2 * D;
    let rows = [
        week_reading(NOW - 2 * D, 200, reset),
        week_reading(NOW - D, 1000, reset),
        week_reading(NOW - H, 1000, reset),
    ];
    let path = crate::harness::account_series::path_of(dir.path(), "alpha", "m1").expect("a path");
    std::fs::create_dir_all(path.parent().expect("its directory")).expect("mkdir");
    let text: String = rows
        .iter()
        .map(|r| r.line().expect("a row") + "\n")
        .collect();
    std::fs::write(&path, text).expect("write");
    let read = Read::new();
    let who = history_who(
        Cli::Claude,
        &read.roster,
        &read.state,
        &Session::default(),
        &read.card().who,
    )
    .expect("a Claude row has a history");
    assert_eq!(who.label.as_deref(), Some("alpha"));
    let held = crate::harness::account_history::read(dir.path(), "m1", &who, 49 * D, utc, NOW);
    let card = read.card_with(HistoryIn::Read {
        read: &held,
        stamp: 7,
    });
    let history = &card.history;
    assert_eq!((history.from, history.stamp), (HistoryFrom::Series, 7));
    assert_eq!(history.since, Some(NOW - 2 * D));
    assert_eq!(
        history
            .days
            .iter()
            .map(|day| (day.left_x10, day.out))
            .collect::<Vec<_>>(),
        [(Some(800), false), (Some(0), true), (Some(0), true),]
    );
    assert_eq!(
        history.reads.iter().map(|r| r.x10).collect::<Vec<_>>(),
        [200, 1000, 1000]
    );
    // A kept history is carried, not rebuilt.
    let kept = Arc::clone(&card.history);
    let again = read.card_with(HistoryIn::Kept(Arc::clone(&kept)));
    assert!(Arc::ptr_eq(&again.history, &kept));
    let empty = HistoryRead::default();
    let none = read.card_with(HistoryIn::Read {
        read: &empty,
        stamp: 9,
    });
    assert_eq!(
        (none.history.from, none.history.stamp),
        (HistoryFrom::Nothing, 9)
    );
    assert!(none.history.days.is_empty() && none.history.reads.is_empty());
    assert_eq!(none.history.since, None, "no row at all");
    // Rows with no reading of the week among them (the plan alone) are a series with nothing
    // to draw and no week to date: nothing found, since their oldest.
    let planned = HistoryRead {
        oldest: Some(NOW - 3 * D),
        ..HistoryRead::default()
    };
    let bare = read.card_with(HistoryIn::Read {
        read: &planned,
        stamp: 11,
    });
    assert_eq!(
        (bare.history.from, bare.history.since, bare.history.stamp),
        (HistoryFrom::Nothing, Some(NOW - 3 * D), 11)
    );
    assert_eq!(
        read.card().history.from,
        HistoryFrom::NotRead,
        "no read made is not a search that found nothing"
    );
    let mut codex = Read::new();
    codex.cli = Cli::Codex;
    codex.state.active = Some("alpha".into());
    assert_eq!(
        codex.card_with(HistoryIn::Kept(kept)).history.from,
        HistoryFrom::NotKept
    );
    assert_eq!(
        history_who(
            Cli::Codex,
            &codex.roster,
            &codex.state,
            &Session::default(),
            &codex.card().who
        ),
        None
    );
}

/// WITH NO SERIES ROW HERE, THE HISTORY IS THE JOURNAL'S WEEK. An account this Mac's series
/// says nothing of draws the lab's readings of its own week over the last eight days, the last
/// of each hour, whether the series read found nothing, was not made, or an earlier history is
/// kept; it has no days and no first row, and keeps the read's stamp. The series wins wherever
/// it holds a row. A kept history follows the journal: the same readings carry it as it is, a
/// new reading replaces it, and a journal that stops saying anything leaves nothing. A reading
/// older than the span, a workspace's and another program's are not drawn.
#[test]
fn with_no_series_row_the_history_is_the_journals_week() {
    crate::harness::journal::with_keyer(keyer, || {
        let reset = NOW + 2 * D;
        let account = |extra: Option<Line>| AccountSeen {
            history: [
                week_line(NOW - 9 * D, "m15", 900, reset - 7 * D),
                week_line(NOW - 3 * D, "m15", 100, reset),
                week_line(NOW - 2 * H - 600, "m20", 300, reset),
                week_line(NOW - 2 * H - 60, "m15", 320, reset),
                Line {
                    sub: true,
                    ..week_line(NOW - H, "m15", 990, reset)
                },
            ]
            .into_iter()
            .chain(extra)
            .collect(),
            ..Default::default()
        };
        let mut read = Read::new();
        read.journal = journal_of("ac-00000000000000a1", account(None));
        let figures = |card: &AccountCard| -> Vec<(i64, u16)> {
            card.history.reads.iter().map(|r| (r.t, r.x10)).collect()
        };
        let drawn = [(NOW - 3 * D, 100), (NOW - 2 * H - 60, 320)];
        let empty = HistoryRead::default();
        let found_none = read.card_with(HistoryIn::Read {
            read: &empty,
            stamp: 9,
        });
        assert_eq!(
            (found_none.history.from, found_none.history.stamp),
            (HistoryFrom::Journal, 9)
        );
        assert_eq!(figures(&found_none), drawn);
        assert!(found_none.history.days.is_empty());
        assert_eq!(found_none.history.since, None);
        assert!(
            found_none
                .history
                .reads
                .iter()
                .all(|r| r.reset == reset && r.epoch.is_none())
        );
        let not_read = read.card();
        assert_eq!(not_read.history.from, HistoryFrom::Journal);
        assert_eq!(figures(&not_read), drawn);
        // A kept history asks the journal again: the same readings are the same history.
        let kept = Arc::clone(&found_none.history);
        let again = read.card_with(HistoryIn::Kept(Arc::clone(&kept)));
        assert!(Arc::ptr_eq(&again.history, &kept));
        read.journal = journal_of(
            "ac-00000000000000a1",
            account(Some(week_line(NOW - 60, "m20", 400, reset))),
        );
        let moved = read.card_with(HistoryIn::Kept(Arc::clone(&kept)));
        assert_eq!(
            (moved.history.from, moved.history.stamp),
            (HistoryFrom::Journal, 9)
        );
        assert_eq!(
            figures(&moved),
            [drawn[0], drawn[1], (NOW - 60, 400)],
            "a new reading is drawn"
        );
        for silent in [
            Journal::Off,
            journal_of("ac-00000000000000a1", AccountSeen::default()),
        ] {
            read.journal = silent;
            let gone = read.card_with(HistoryIn::Kept(Arc::clone(&kept)));
            assert_eq!(
                (gone.history.from, gone.history.stamp),
                (HistoryFrom::Nothing, 9)
            );
            assert!(gone.history.reads.is_empty());
        }
        // CONTROL: a series row here is the history, whatever the journal holds.
        read.journal = journal_of("ac-00000000000000a1", account(None));
        let series = Arc::new(History {
            reads: vec![outlook::Read {
                t: NOW - H,
                x10: 500,
                reset,
                epoch: None,
            }],
            since: Some(NOW - H),
            stamp: 3,
            ..History::none(HistoryFrom::Series)
        });
        let own = read.card_with(HistoryIn::Kept(Arc::clone(&series)));
        assert!(Arc::ptr_eq(&own.history, &series));
        read.cli = Cli::Codex;
        read.state.active = Some("alpha".into());
        assert_eq!(read.card().history.from, HistoryFrom::NotKept);
    });
}

/// A JOURNAL STAND-IN KEEPS THE SERIES' FIRST ROW. A series here with rows of the account but no
/// week reading inside the span (its plan alone) gives way to the journal's readings, and the
/// history kept from that build keeps the series' oldest row: when the journal later says
/// nothing, the kept history is the series' "nothing in the span", never "no row kept here"
/// (the defect: the journal stand-in dropped `since`, so a kept one later read as no row at
/// all). CONTROL: the same journal over a series with no row at all keeps no `since`.
#[test]
fn a_journal_stand_in_keeps_the_series_first_row() {
    crate::harness::journal::with_keyer(keyer, || {
        let reset = NOW + 2 * D;
        let mut read = Read::new();
        read.journal = journal_of(
            "ac-00000000000000a1",
            AccountSeen {
                history: vec![week_line(NOW - 3 * D, "m15", 100, reset)],
                ..Default::default()
            },
        );
        let planned = HistoryRead {
            oldest: Some(NOW - 30 * D),
            ..HistoryRead::default()
        };
        let lab = read.card_with(HistoryIn::Read {
            read: &planned,
            stamp: 11,
        });
        assert_eq!(
            (lab.history.from, lab.history.since),
            (HistoryFrom::Journal, Some(NOW - 30 * D)),
            "the journal stands in, the series' first row kept"
        );
        read.journal = journal_of("ac-00000000000000a1", AccountSeen::default());
        let later = read.card_with(HistoryIn::Kept(Arc::clone(&lab.history)));
        assert_eq!(
            (later.history.from, later.history.since),
            (HistoryFrom::Nothing, Some(NOW - 30 * D)),
            "kept, then the journal silent: rows here, none in the span"
        );
        let fresh = read.card_with(HistoryIn::Read {
            read: &planned,
            stamp: 11,
        });
        assert_eq!(
            fresh.history.since, later.history.since,
            "the kept history says what a fresh fold says"
        );
        let empty = HistoryRead::default();
        read.journal = journal_of(
            "ac-00000000000000a1",
            AccountSeen {
                history: vec![week_line(NOW - 3 * D, "m15", 100, reset)],
                ..Default::default()
            },
        );
        let none = read.card_with(HistoryIn::Read {
            read: &empty,
            stamp: 9,
        });
        assert_eq!(
            (none.history.from, none.history.since),
            (HistoryFrom::Journal, None),
            "CONTROL: no row here, no first row"
        );
    });
}

/// AN OUTSIDE LOGIN'S HISTORY IS ASKED FOR BY ITS KEYS. It has no label here, so its series
/// rows are found by the lab key its address gives and the account key its profile names; with
/// neither it is still asked for, and finds nothing.
#[test]
fn an_outside_logins_history_is_asked_for_by_key() {
    let read = Read::outside();
    let who = read.card().who;
    let key = AccountKey::claude("5d0c1f2e-3a4b-4c5d-8e9f-0a1b2c3d4e5f").expect("a key");
    let session = Session {
        label: None,
        email: read.email,
        profile_key: Some(key),
    };
    let asked = crate::harness::journal::with_keyer(keyer, || {
        history_who(Cli::Claude, &read.roster, &read.state, &session, &who)
    })
    .expect("an outside login has a history to look for");
    assert_eq!(
        asked,
        HistoryWho {
            label: None,
            pk: Some("ac-00000000000000c3".into()),
            key: Some(key),
        }
    );
    let unkeyed = history_who(
        Cli::Claude,
        &read.roster,
        &read.state,
        &Session::default(),
        &who,
    );
    assert_eq!(unkeyed, Some(HistoryWho::default()));
}
