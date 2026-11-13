// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The token view and the quiet offer (DESIGN-claude-setup-token §5.9, §5.10, slice S12): the
//! offer once per change of its pair, none while a write made here waits, the step's
//! `--replace`, the report's fields, peers from a fleet read, the state line's phrase and the
//! doctor's counts. The checks are of structure (records, keys, steps), never of wording.

use super::*;
use crate::harness::accounts_tokens::{NOTED, Table, UsedOn, Verification};
use crate::harness::journal;
use aterm_secrets::claude_token::{Check, ExpiryFrom, Meta, Verified};
use std::cell::RefCell;

/// The clock (2027-01-15).
const NOW: u64 = 1_800_000_000;
const DAY: u64 = 86_400;
const KEY: &str = "ac-fedcba9876543210";
const EMAIL: &str = "person@example.com";

fn key() -> Key {
    Key::parse(KEY).expect("a key")
}

fn scratch() -> (aterm_tempfile::TempDir, Store) {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-token-view-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch");
    let store = Store::at(dir.path());
    (dir, store)
}

fn covered(login: LoginHalf) -> Covered {
    Covered {
        key: key(),
        email: EMAIL.to_owned(),
        label: Some("work".to_owned()),
        logged_in: true,
        login,
    }
}

/// A verified token, made a day ago on m7, ending at `expires_at`, handed to `exposed`.
fn standing(expires_at: u64, exposed: &[&str]) -> Standing {
    Standing {
        key: key(),
        meta: Some(Meta {
            email: EMAIL.to_owned(),
            account_id: "c-0123456789ab".to_owned(),
            minted_at: NOW - DAY,
            expires_at,
            expiry_from: ExpiryFrom::Printed,
            minted_on: "m7".to_owned(),
            cli: "2.1.318".to_owned(),
            verified: Some(Verified {
                on: "m7".to_owned(),
                at: NOW - 3_600,
                by: "windows".to_owned(),
                seven_day_resets_at: None,
                five_hour_resets_at: None,
            }),
        }),
        check: Check::Readable,
        signer: Some("m7".to_owned()),
        minted_here: false,
        exposed: exposed.iter().map(|m| (*m).to_owned()).collect(),
    }
}

fn offered_notes() -> usize {
    NOTED.with(|noted| {
        std::mem::take(&mut *noted.borrow_mut())
            .into_iter()
            .filter(|note| note.kind == journal::TokenEvent::Offered.kind())
            .count()
    })
}

/// THE PAIR'S SEQUENCE `none/ok` → `none/ending` → `soon/-` YIELDS THREE `token-offered`
/// RECORDS, AND RE-READS YIELD NONE: each change of either half is a new pair, the same pair is
/// recorded once, and the token in use turns the login half to `-`.
#[test]
fn the_pair_sequence_offers_three_times_and_rereads_none() {
    let (_dir, store) = scratch();
    let _ = offered_notes();
    let ending = LoginHalf::Ending {
        end: i64::try_from(NOW + 3 * DAY).unwrap(),
    };
    let steps: [(LoginHalf, Lookup<Standing>, &str); 3] = [
        (LoginHalf::Ok, Lookup::Absent, "ok"),
        (ending, Lookup::Absent, "ending"),
        (ending, Lookup::Found(standing(NOW + 10 * DAY, &[])), "-"),
    ];
    let mut pairs = Vec::new();
    for (login, found, half) in steps {
        for reread in 0..3 {
            let mut accounts = vec![account(covered(login), &found, None, Some(false), NOW)];
            let offer = accounts[0].offer.clone().expect("an offer stands");
            assert_eq!(accounts[0].login.word(), half);
            let noted = note_offers(&store, &mut accounts, NOW + reread).expect("noted");
            assert_eq!(noted, usize::from(reread == 0), "{half}, read {reread}");
            assert_eq!(
                accounts[0].offer.as_ref().and_then(|o| o.since),
                Some(NOW),
                "since is the first offer of the pair"
            );
            if reread == 0 {
                pairs.push(offer.pair);
            }
        }
    }
    assert_eq!(offered_notes(), 3, "{pairs:?}");
    let distinct: std::collections::BTreeSet<_> = pairs.iter().collect();
    assert_eq!(distinct.len(), 3, "{pairs:?}");
    let table = tokens::read_table(&store).expect("the table");
    assert_eq!(
        table.offers.get(KEY).map(|o| o.pair.as_str()),
        pairs.last().map(String::as_str)
    );
    // CONTROL: an account no session here is on is offered nothing.
    let away = Covered {
        logged_in: false,
        ..covered(LoginHalf::Ok)
    };
    assert!(
        account(away, &Lookup::Absent, None, Some(false), NOW)
            .offer
            .is_none()
    );
}

/// A TOKEN WRITE WAITING HERE OFFERS NOTHING WHILE ITS STATE STILL READS `none`, AND THE REPORT
/// SAYS "NOT YET IN VAULT": with the waiting write unread, nothing is offered either; once the
/// write is not waiting, the same state offers.
#[test]
fn a_write_waiting_here_offers_nothing_and_says_not_yet_in_vault() {
    let waiting = account(
        covered(LoginHalf::Ok),
        &Lookup::Absent,
        None,
        Some(true),
        NOW,
    );
    assert_eq!(waiting.state, TokenState::None);
    assert!(waiting.offer.is_none());
    assert!(waiting.not_yet());
    assert!(waiting.phrase(NOW as i64).is_none());
    let report = Report::Accounts {
        accounts: vec![waiting],
        peers: PeersShown::NotRead,
        sessions: None,
    };
    let text = report.text();
    assert!(text.contains("not yet in vault"), "{text}");
    assert!(!text.contains("offer:"), "{text}");
    let json = report.json();
    assert_eq!(json["accounts"][0]["not_yet_in_vault"], Value::from(true));
    assert_eq!(json["accounts"][0]["offer"], Value::Null);
    let unread = account(covered(LoginHalf::Ok), &Lookup::Absent, None, None, NOW);
    assert!(unread.offer.is_none(), "an unread wait offers nothing");
    // CONTROL: not waiting, it offers.
    let free = account(
        covered(LoginHalf::Ok),
        &Lookup::Absent,
        None,
        Some(false),
        NOW,
    );
    assert!(free.offer.is_some());
}

/// THE STEP FOR `soon` AND `exposed` CARRIES `--replace`; for `none`, `ended` and `rejected` it is
/// a bare run; `active` and `unknown` offer nothing.
#[test]
fn the_step_for_soon_and_exposed_carries_replace() {
    /// A world: its name, the vault's standing, this Mac's row, and whether the step replaces.
    type Case = (
        &'static str,
        Lookup<Standing>,
        Option<TokenRow>,
        Option<bool>,
    );
    let rejected = TokenRow {
        rejected_at: Some(NOW - 60),
        ..row()
    };
    let cases: [Case; 7] = [
        (
            "soon",
            Lookup::Found(standing(NOW + DAY, &[])),
            None,
            Some(true),
        ),
        (
            "exposed",
            Lookup::Found(standing(NOW + 300 * DAY, &["m9"])),
            None,
            Some(true),
        ),
        ("none", Lookup::Absent, None, Some(false)),
        (
            "ended",
            Lookup::Found(standing(NOW - 1, &[])),
            None,
            Some(false),
        ),
        (
            "rejected",
            Lookup::Found(standing(NOW + 300 * DAY, &[])),
            Some(rejected),
            Some(false),
        ),
        (
            "active",
            Lookup::Found(standing(NOW + 300 * DAY, &[])),
            None,
            None,
        ),
        ("unknown", Lookup::Unknown, None, None),
    ];
    for (name, found, row, replace) in cases {
        let shown = account(covered(LoginHalf::Ok), &found, row, Some(false), NOW);
        assert_eq!(shown.state.word(), name);
        let step = shown.offer.as_ref().map(|offer| offer.step.clone());
        match replace {
            Some(replace) => assert_eq!(
                step,
                Some(Step::Token {
                    label: "work".to_owned(),
                    replace,
                }),
                "{name}"
            ),
            None => assert_eq!(step, None, "{name}"),
        }
    }
}

fn row() -> TokenRow {
    TokenRow {
        key: KEY.to_owned(),
        email: EMAIL.to_owned(),
        account_id: Some("c-0123456789ab".to_owned()),
        label: Some("work".to_owned()),
        entry: key().entry(),
        minted_at: Some(NOW - DAY),
        expires_at: Some(NOW + 300 * DAY),
        expiry_from: Some("printed".to_owned()),
        minted_on: Some("m7".to_owned()),
        signer: Some("m7".to_owned()),
        used_on: UsedOn {
            first_served_at: Some(NOW - DAY),
            last_served_at: Some(NOW),
            peers: vec!["m9".to_owned()],
            peers_read_at: Some(NOW - DAY),
        },
        vault: InVault::InVault,
        verified: Some(NOW - 3_600),
        verification: Verification::default(),
        rejected_at: None,
        removed_at: None,
    }
}

/// THE REPORT'S TEXT AND JSON CARRY THE FIELDS: every owner-named field of an account with a
/// token, and its sessions with their routes.
#[test]
fn the_report_carries_the_fields() {
    let shown = account(
        covered(LoginHalf::Ok),
        &Lookup::Found(standing(NOW + 300 * DAY, &[])),
        Some(row()),
        Some(false),
        NOW,
    );
    let entry = shown.entry();
    let report = Report::Accounts {
        accounts: vec![shown],
        peers: PeersShown::Read,
        sessions: Some(sessions_of(&[format!(
            "session local=1 sid=s-1 account=work auth=token token={KEY}"
        )])),
    };
    let text = report.text();
    for field in [entry.as_str(), "m7", "@s-1", "token"] {
        assert!(text.contains(field), "{field}: {text}");
    }
    let json = report.json();
    let account = &json["accounts"][0];
    for field in [
        "key",
        "email",
        "label",
        "state",
        "entry",
        "minted_at",
        "expires_at",
        "expiry_from",
        "minted_on",
        "signer",
        "used_on",
        "vault",
        "verified",
        "rejected_at",
        "offer",
        "login",
    ] {
        assert!(account.get(field).is_some(), "{field}: {json}");
    }
    assert_eq!(account["state"], Value::from("active"));
    assert_eq!(account["entry"], Value::from(entry));
    assert_eq!(account["used_on"]["peers"][0], Value::from("m9"));
    assert_eq!(account["used_on"]["peers_read"], Value::from(true));
    assert_eq!(json["sessions"][0]["route"], Value::from("token"));
    assert_eq!(json["sessions"][0]["token"], Value::from(KEY));
}

fn peer(label: &str, answer: Result<Vec<String>, String>) -> aterm_ctl::PeerSurvey {
    aterm_ctl::PeerSurvey {
        authenticated: None,
        label: label.to_owned(),
        answer,
        fault: aterm_ctl::PeerFault::Other,
        unproven: false,
        node: None,
        addr: None,
        reason: None,
    }
}

/// A FIXTURE FLEET READ FOLDS A PEER ROW'S `token=` KEY INTO `used_on`, AND AN OFFLINE READ SAYS
/// "PEERS NOT READ": a peer answering with a token session is named for the key, one answering
/// with a stored session or an older row is not, a peer that did not answer names nothing, and
/// a read no peer answered is no read.
#[test]
fn a_fleet_read_folds_peer_token_keys_and_offline_says_not_read() {
    let read = aterm_ctl::FleetRead {
        peers: vec![
            peer(
                "m7",
                Ok(vec![
                    format!("session local=1 sid=s-1 auth=token token={KEY}"),
                    format!("session local=2 sid=s-2 auth=token token={KEY}"),
                ]),
            ),
            peer(
                "m8",
                Ok(vec![
                    "session local=1 sid=s-3 auth=stored token=-".to_owned(),
                    "session local=2 sid=s-4 state=live".to_owned(),
                ]),
            ),
            peer("m9", Err("no route".to_owned())),
        ],
        note: None,
        alone: None,
    };
    let peers = peers_of(&read, NOW);
    let Peers::Read { at, by_key } = &peers else {
        panic!("a read: {peers:?}");
    };
    assert_eq!(*at, NOW);
    assert_eq!(by_key.get(KEY), Some(&vec!["m7".to_owned()]));
    assert_eq!(by_key.len(), 1);
    let (_dir, store) = scratch();
    tokens::update_table(&store, |table: &mut Table| {
        table.rows.insert(
            KEY.to_owned(),
            TokenRow {
                used_on: UsedOn::default(),
                ..row()
            },
        );
    })
    .expect("a row");
    note_peers(&store, &peers).expect("noted");
    let kept = tokens::read_table(&store).expect("the table");
    let used = &kept.rows[KEY].used_on;
    assert_eq!(used.peers, vec!["m7".to_owned()]);
    assert_eq!(used.peers_read_at, Some(NOW));

    let offline = aterm_ctl::FleetRead {
        peers: vec![peer("m7", Err("timed out".to_owned()))],
        note: None,
        alone: None,
    };
    assert_eq!(peers_of(&offline, NOW + 1), Peers::NotRead);
    note_peers(&store, &Peers::NotRead).expect("nothing");
    assert_eq!(tokens::read_table(&store).expect("the table"), kept);
    let shown = account(
        covered(LoginHalf::Ok),
        &Lookup::Found(standing(NOW + 300 * DAY, &[])),
        kept.rows.get(KEY).cloned(),
        Some(false),
        NOW,
    );
    let json = shown.json(&PeersShown::NotRead);
    assert_eq!(json["used_on"]["peers_read"], Value::from(false));
    assert_eq!(json["used_on"]["peers_read_at"], Value::from(NOW));
    assert!(shown.text(&PeersShown::NotRead).contains("peers not read"));
}

/// THE STATE LINE HOLDS THE PHRASE ONLY WHILE AN OFFER STANDS: an account with no token in use
/// carries one, with its login's end where the login is ending; one whose token is active
/// carries none.
#[test]
fn the_phrase_stands_only_while_an_offer_stands() {
    let now = i64::try_from(NOW).unwrap();
    let plain = account(
        covered(LoginHalf::Ok),
        &Lookup::Absent,
        None,
        Some(false),
        NOW,
    );
    assert_eq!(plain.phrase(now).as_deref(), Some("token: none"));
    let ending = account(
        covered(LoginHalf::Ending {
            end: now + 5 * 86_400,
        }),
        &Lookup::Absent,
        None,
        Some(false),
        NOW,
    );
    let phrase = ending.phrase(now).expect("a phrase");
    assert!(
        phrase.contains('5') && phrase.ends_with("token: none"),
        "{phrase}"
    );
    let active = account(
        covered(LoginHalf::Ending {
            end: now + 5 * 86_400,
        }),
        &Lookup::Found(standing(NOW + 300 * DAY, &[])),
        None,
        Some(false),
        NOW,
    );
    assert!(active.phrase(now).is_none());
    assert!(active.offer_line().is_none());
    assert!(plain.offer_line().is_some());
}

/// AN OLDER AT ERM'S SURVEY ROW WITHOUT `auth=` IS LEFT OUT OF THE SESSIONS; one with it reads.
#[test]
fn a_survey_row_without_auth_is_left_out() {
    let rows = sessions_of(&[
        "session local=1 sid=s-1 state=live account=work".to_owned(),
        "session local=2 sid=s-2 account=work auth=stored token=-".to_owned(),
        "instance node=n1".to_owned(),
    ]);
    assert_eq!(
        rows,
        vec![SessionRoute {
            sid: "s-2".to_owned(),
            account: Some("work".to_owned()),
            auth: "stored".to_owned(),
            token: None,
            login: None,
        }]
    );
}

/// THE DOCTOR'S WORDS COUNT THE STATES: one count per state named, each the number of accounts in
/// it.
#[test]
fn the_doctor_words_count_the_states() {
    let accounts = vec![
        account(
            covered(LoginHalf::Ok),
            &Lookup::Found(standing(NOW + 300 * DAY, &[])),
            None,
            Some(false),
            NOW,
        ),
        account(
            covered(LoginHalf::Ok),
            &Lookup::Found(standing(NOW + DAY, &[])),
            None,
            Some(false),
            NOW,
        ),
        account(
            covered(LoginHalf::Ok),
            &Lookup::Absent,
            None,
            Some(true),
            NOW,
        ),
    ];
    let words = doctor_words(&accounts);
    let counts: Vec<u32> = words
        .split(&[':', ','][..])
        .filter_map(|part| part.split_whitespace().next()?.parse().ok())
        .collect();
    assert_eq!(counts, vec![1, 1, 0, 1, 0], "{words}");
    // A vault whose read failed knows no account's standing: it says so, never counts that would
    // read as a vault that holds no token.
    let mut unread = accounts;
    unread.push(account(
        covered(LoginHalf::Ok),
        &Lookup::Unknown,
        None,
        None,
        NOW,
    ));
    assert_eq!(
        doctor_words(&unread),
        "claude tokens: unknown (vault not read)"
    );
}

/// THE VIEW READS THE VAULT ONCE AND COVERS A ROW-LESS SESSION LOGIN: with no roster, a login
/// record's account is covered by its own email, and the vault's `listed` read is the one read.
/// Its offer stands only where the host's verb makes a token ([`Places::offers`]).
#[test]
fn the_view_covers_a_rowless_session_login_in_one_read() {
    struct Once {
        listed: RefCell<u32>,
    }
    impl TokenVault for Once {
        fn route(
            &self,
            _: std::time::Instant,
        ) -> Result<aterm_secrets::client::WriteRoute, String> {
            Err("not asked".to_owned())
        }
        fn standing(&self, _: &Key, _: std::time::Instant) -> Result<Lookup<Standing>, String> {
            panic!("the view reads every account in one listed read")
        }
        fn listed(&self) -> Result<Lookup<Vec<Standing>>, String> {
            *self.listed.borrow_mut() += 1;
            Ok(Lookup::Found(Vec::new()))
        }
        fn waiting_here(&self, _: &Key) -> Result<bool, String> {
            Ok(false)
        }
        fn seat_here(&self) -> Result<Option<String>, String> {
            Ok(None)
        }
        fn write(
            &self,
            _: &Key,
            _: &aterm_secrets::SecretBytes,
            _: &Meta,
            _: aterm_secrets::client::OnBusy,
        ) -> Result<aterm_secrets::client::MemberWritten, String> {
            panic!("no write")
        }
        fn remove(
            &self,
            _: &Key,
            _: std::time::Instant,
        ) -> Result<aterm_secrets::client::MemberUnset, String> {
            panic!("no removal")
        }
        fn verify(
            &self,
            _: &Key,
            _: Verified,
            _: aterm_secrets::client::OnBusy,
        ) -> Result<aterm_secrets::client::MemberWritten, String> {
            panic!("no verification")
        }
    }
    let (_dir, store) = scratch();
    let logins = vec![SessionLogin {
        email: EMAIL.to_owned(),
        account_id: None,
        default: true,
    }];
    let vault = Once {
        listed: RefCell::new(0),
    };
    let shown_where = |offers: bool| {
        journal::with_keyer(
            |_, _| Some(KEY.to_owned()),
            || {
                view(
                    &Places {
                        store: &store,
                        roster: None,
                        logins: &logins,
                        now: i64::try_from(NOW).unwrap(),
                        offers,
                    },
                    &vault,
                )
            },
        )
    };
    let shown = shown_where(true);
    assert_eq!(*vault.listed.borrow(), 1);
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].key, KEY);
    assert_eq!(shown[0].label, None);
    assert_eq!(
        shown[0].offer.as_ref().map(|offer| offer.step.clone()),
        Some(Step::Token {
            label: EMAIL.to_owned(),
            replace: false,
        })
    );
    // CONTROL: a host whose verb makes no token (off macOS) is offered no step, and still says
    // the token's state.
    let elsewhere = shown_where(false);
    assert_eq!(elsewhere.len(), 1);
    assert_eq!(elsewhere[0].state, TokenState::None);
    assert!(elsewhere[0].offer.is_none(), "{:?}", elsewhere[0].offer);
    assert!(elsewhere[0].phrase(i64::try_from(NOW).unwrap()).is_none());
}

/// A vault whose post a server takes where `takes`, counting the posts it was asked for; it
/// answers nothing else.
struct Posting {
    takes: bool,
    posts: RefCell<u32>,
}

impl TokenVault for Posting {
    fn route(&self, _: std::time::Instant) -> Result<aterm_secrets::client::WriteRoute, String> {
        Err("not asked".to_owned())
    }
    fn standing(&self, _: &Key, _: std::time::Instant) -> Result<Lookup<Standing>, String> {
        Err("not asked".to_owned())
    }
    fn listed(&self) -> Result<Lookup<Vec<Standing>>, String> {
        Err("not asked".to_owned())
    }
    fn waiting_here(&self, _: &Key) -> Result<bool, String> {
        Ok(false)
    }
    fn seat_here(&self) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn write(
        &self,
        _: &Key,
        _: &aterm_secrets::SecretBytes,
        _: &Meta,
        _: aterm_secrets::client::OnBusy,
    ) -> Result<aterm_secrets::client::MemberWritten, String> {
        panic!("no write")
    }
    fn remove(
        &self,
        _: &Key,
        _: std::time::Instant,
    ) -> Result<aterm_secrets::client::MemberUnset, String> {
        panic!("no removal")
    }
    fn verify(
        &self,
        _: &Key,
        _: Verified,
        _: aterm_secrets::client::OnBusy,
    ) -> Result<aterm_secrets::client::MemberWritten, String> {
        panic!("no verification")
    }
    fn post(&self) -> Result<bool, String> {
        *self.posts.borrow_mut() += 1;
        Ok(self.takes)
    }
}

/// "NOT YET IN VAULT" TURNS ONCE A SERVER TAKES THE WRITE (§5.1, §3.10): a token this Mac made
/// while no server answered reads `signed here`; a surface's read retries the idempotent post, at
/// most once a minute, and once a server takes it the row reads `in vault`, in the table too, and
/// is no longer counted not yet in vault. CONTROL: while no server takes it, the row stays
/// `signed here`, and a read within the minute posts nothing more.
///
/// NEGATIVE CONTROL (2026-11-06, restored with cp from a backup plus touch, confirmed with cmp):
/// with `note_posted` posting nothing, `the read retried the post` reads `left: 0`, `right: 1`.
#[test]
fn a_signed_here_row_turns_in_vault_once_a_server_takes_its_write() {
    let (_dir, store) = scratch();
    tokens::update_table(&store, |table: &mut Table| {
        table.rows.insert(
            KEY.to_owned(),
            TokenRow {
                vault: InVault::SignedHere,
                ..row()
            },
        );
    })
    .expect("a row");
    let read = || {
        vec![account(
            covered(LoginHalf::Ok),
            &Lookup::Found(standing(NOW + 300 * DAY, &[])),
            tokens::read_table(&store)
                .expect("the table")
                .rows
                .get(KEY)
                .cloned(),
            Some(false),
            NOW,
        )]
    };
    let offline = Posting {
        takes: false,
        posts: RefCell::new(0),
    };
    let mut accounts = read();
    assert!(accounts[0].not_yet(), "CONTROL: signed here");
    assert_eq!(
        note_posted(&store, &mut accounts, &offline, NOW).unwrap(),
        0
    );
    assert_eq!(*offline.posts.borrow(), 1, "the read retried the post");
    assert!(accounts[0].not_yet(), "CONTROL: no server took it");
    let mut again = read();
    assert_eq!(
        note_posted(&store, &mut again, &offline, NOW + POST_EVERY_S - 1).unwrap(),
        0
    );
    assert_eq!(*offline.posts.borrow(), 1, "at most once a minute");

    let online = Posting {
        takes: true,
        posts: RefCell::new(0),
    };
    let mut accounts = read();
    assert_eq!(
        note_posted(&store, &mut accounts, &online, NOW + POST_EVERY_S).unwrap(),
        1
    );
    assert!(!accounts[0].not_yet(), "{:?}", accounts[0].vault_words());
    assert_eq!(accounts[0].vault_words().as_deref(), Some("in vault"));
    let kept = tokens::read_table(&store).expect("the table");
    assert_eq!(kept.rows[KEY].vault, InVault::InVault);
    assert!(kept.posts.is_empty(), "no retry is kept once it turned");
    assert!(!read()[0].not_yet(), "a later read reads it in vault");
    assert_eq!(
        doctor_words(&read()).matches("0 not yet in vault").count(),
        1
    );
}

/// EACH SESSION NAMES THE ACCOUNT ITS LOGIN RECORD NAMES (§5.2, §5.10): a session on no row runs
/// on the machine's default login, so the report names that record's account and its token state
/// beside the route, in its text and its JSON; one on a row names the row's account, and one on
/// the token route the token's. CONTROL: with no default login record here, a session on no row
/// names none.
///
/// NEGATIVE CONTROL (2026-11-06, restored with cp from a backup plus touch, confirmed with cmp):
/// with `with_logins` naming no login for a session on no row, `@s-1` reads `login: None`.
#[test]
fn each_session_names_the_account_its_login_record_names() {
    let shown = account(
        covered(LoginHalf::Ok),
        &Lookup::Absent,
        None,
        Some(false),
        NOW,
    );
    let rows = sessions_of(&[
        "session local=1 sid=s-1 auth=stored token=-".to_owned(),
        "session local=2 sid=s-2 account=work auth=stored token=-".to_owned(),
        format!("session local=3 sid=s-3 auth=token token={KEY}"),
    ]);
    let logins = [SessionLogin {
        email: EMAIL.to_owned(),
        account_id: None,
        default: true,
    }];
    let named = journal::with_keyer(
        |_, _| Some(KEY.to_owned()),
        || with_logins(rows.clone(), std::slice::from_ref(&shown), &logins),
    );
    assert!(
        named
            .iter()
            .all(|session| session.login.as_deref() == Some(KEY)),
        "{named:?}"
    );
    let report = Report::Accounts {
        accounts: vec![shown.clone()],
        peers: PeersShown::NotRead,
        sessions: Some(named),
    };
    let text = report.text();
    assert!(
        text.contains(&format!(
            "@s-1 stored (logged into {EMAIL} {KEY}, token: none)"
        )),
        "{text}"
    );
    let json = report.json();
    assert_eq!(json["sessions"][0]["login"]["key"], Value::from(KEY));
    assert_eq!(json["sessions"][0]["login"]["email"], Value::from(EMAIL));
    assert_eq!(json["sessions"][0]["login"]["token"], Value::from("none"));
    let unnamed = journal::with_keyer(
        |_, _| Some(KEY.to_owned()),
        || with_logins(rows, std::slice::from_ref(&shown), &[]),
    );
    assert_eq!(unnamed[0].login, None, "CONTROL: no default login record");
    assert_eq!(unnamed[1].login.as_deref(), Some(KEY), "CONTROL: the row's");
}
