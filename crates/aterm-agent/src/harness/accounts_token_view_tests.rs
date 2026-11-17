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
        login: LoginHere::Half(login),
        login_turns: None,
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
            minted_by: Some(aterm_secrets::claude_token::MintedBy::Window),
            verified: Some(Verified {
                on: "m7".to_owned(),
                at: NOW - 3_600,
                by: aterm_secrets::claude_token::BY_WINDOWS.to_owned(),
                seven_day_resets_at: None,
                five_hour_resets_at: None,
            }),
        }),
        check: Check::Readable,
        signer: Some("m7".to_owned()),
        minted_here: false,
        exposed: exposed.iter().map(|m| (*m).to_owned()).collect(),
        refused_at: None,
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
        steps: Held::default(),
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

/// THE STEP FOR `soon` AND `exposed` CARRIES `--replace`, and so does the one for a token rejected
/// here that the vault's records put in use on other Macs (this one is verified); for `none` and
/// `ended` it is a bare run; `active` and `unknown` offer nothing.
#[test]
fn the_step_for_soon_and_exposed_carries_replace() {
    /// A world: its name, the vault's standing, this Mac's row, and whether the step replaces.
    type Case = (
        &'static str,
        Lookup<Standing>,
        Option<TokenRow>,
        Option<bool>,
    );
    let rejected = Standing {
        refused_at: Some(NOW - 60),
        ..standing(NOW + 300 * DAY, &[])
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
        ("rejected", Lookup::Found(rejected), Some(row()), Some(true)),
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
        steps: Held::default(),
    };
    let text = report.text();
    for field in [entry.as_str(), "m7 in aterm's window", "@s-1", "token"] {
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
        "minted_by",
        "signer",
        "used_on",
        "vault",
        "verified",
        "verified_on",
        "rejected_at",
        "offer",
        "login",
    ] {
        assert!(account.get(field).is_some(), "{field}: {json}");
    }
    assert_eq!(account["state"], Value::from("active"));
    assert_eq!(account["minted_by"], Value::from("window"));
    assert_eq!(account["entry"], Value::from(entry));
    assert_eq!(account["used_on"]["peers"][0], Value::from("m9"));
    assert_eq!(account["used_on"]["peers_read"], Value::from(true));
    assert_eq!(json["sessions"][0]["route"], Value::from("token"));
    assert_eq!(json["sessions"][0]["token"], Value::from(KEY));
}

/// A TOKEN'S VERIFICATION IS SHOWN FROM THE VAULT'S STAMP, WITH THE MAC THAT WROTE IT (FL-134):
/// any Mac that holds the account's stored login verifies a token, so the Mac showing it need not
/// be the one that wrote `verified`. This Mac's row says nothing (m12 verified the token), or says
/// its own write's time (its stamp lost the settle to m12's): either way the report's text and
/// JSON carry the vault's stamp, when and on which Mac. A pair that does not read as one token
/// shows no verification. Where the vault was not read, the row's own time stands, naming no Mac.
/// NEGATIVE CONTROL (2026-11-15, by hand, then restored): with the report reading
/// `row.verified`, "another Mac's stamp" fails.
#[test]
fn a_verification_is_shown_from_the_vaults_stamp_with_the_mac_that_wrote_it() {
    let then = NOW - 2 * DAY;
    let stamped = || {
        let mut standing = standing(NOW + 300 * DAY, &[]);
        let verified = standing
            .meta
            .as_mut()
            .and_then(|meta| meta.verified.as_mut())
            .expect("a stamp");
        (verified.on, verified.at) = ("m12".to_owned(), then);
        standing
    };
    let shown_with = |found: &Lookup<Standing>, verified: Option<u64>| {
        account(
            covered(LoginHalf::Ok),
            found,
            Some(TokenRow { verified, ..row() }),
            Some(false),
            NOW,
        )
    };
    let report = |shown: AccountToken| Report::Accounts {
        accounts: vec![shown],
        peers: PeersShown::Read,
        sessions: None,
        steps: Held::default(),
    };
    let by_m12 = Lookup::Found(stamped());
    for (why, row_says) in [
        ("another Mac's stamp", None),
        ("the vault's stamp, not this Mac's write", Some(NOW - 3_600)),
    ] {
        let report = report(shown_with(&by_m12, row_says));
        let words = format!("verified {} on m12", date(then));
        assert!(report.text().contains(&words), "{why}: {}", report.text());
        let json = report.json();
        assert_eq!(json["accounts"][0]["verified"], Value::from(then), "{why}");
        assert_eq!(
            json["accounts"][0]["verified_on"],
            Value::from("m12"),
            "{why}"
        );
    }
    let mut unsettled = stamped();
    unsettled.check = Check::Mismatched(aterm_secrets::claude_token::Mismatch::Draft);
    let text = report(shown_with(&Lookup::Found(unsettled), Some(NOW - 3_600))).text();
    assert!(
        !text.contains("verified "),
        "no token read, so none verified: {text}"
    );
    let unread = report(shown_with(&Lookup::Unknown, Some(NOW - 3_600)));
    let text = unread.text();
    assert!(
        text.contains(&format!("verified {};", date(NOW - 3_600))),
        "the vault not read: this Mac's row, naming no Mac: {text}"
    );
    assert_eq!(unread.json()["accounts"][0]["verified_on"], Value::Null);
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

/// A ROW IS SAID TO HOLD A LOGIN ONLY WHERE THIS MAC HOLDS ONE: a row's records forecast nothing
/// both for a login with no end known and for no login at all, so the half alone reads `ok` of
/// either. The row's login here is that half where the Mac holds a login of it or a record says
/// one ended, and `none` otherwise; a table row with no roster row is `unread`, and the report
/// carries each word. CONTROL: a held login keeps its half and reports `ok`.
#[test]
fn a_row_with_no_login_here_is_not_said_to_hold_one() {
    let ended = LoginHalf::Ended { at: None };
    assert_eq!(LoginHere::of_row(LoginHalf::Ok, false), LoginHere::Absent);
    assert_eq!(
        LoginHere::of_row(LoginHalf::Ok, true),
        LoginHere::Half(LoginHalf::Ok),
        "CONTROL: a login held here"
    );
    assert_eq!(
        LoginHere::of_row(ended, false),
        LoginHere::Half(ended),
        "a recorded end is said of a row with no login now"
    );
    let word_of = |login: LoginHere, logged_in: bool| {
        let covered = Covered {
            login,
            logged_in,
            ..covered(LoginHalf::Ok)
        };
        let shown = account(covered, &Lookup::Absent, None, Some(false), NOW);
        assert_eq!(shown.login, login);
        shown.json(&PeersShown::NotRead)["login"].clone()
    };
    assert_eq!(word_of(LoginHere::Absent, false), Value::from("none"));
    assert_eq!(word_of(LoginHere::Unread, false), Value::from("unread"));
    assert_eq!(
        word_of(LoginHere::Half(LoginHalf::Ok), true),
        Value::from("ok"),
        "CONTROL: a login here"
    );
}

/// The lab keys of [`the_roster_of_three`]'s accounts, by email.
fn keyed_by_email(_realm: &str, email: &str) -> Option<String> {
    let key = match email {
        "home@example.com" => "ac-0000000000000001",
        "made@example.com" => "ac-0000000000000002",
        "bare@example.com" => "ac-0000000000000003",
        _ => return None,
    };
    Some(key.to_owned())
}

/// A roster under `dir` of three rows: `home` on the default login (no directory), `made`
/// whose own directory holds a login, and `bare` whose directory holds none.
fn the_roster_of_three(dir: &Path) -> std::path::PathBuf {
    let made = dir.join("made");
    std::fs::create_dir_all(&made).expect("made");
    std::fs::write(made.join(".claude.json"), "{}").expect("a login file");
    let roster = dir.join("accounts.toml");
    let row = |label: &str, at: Option<&Path>| {
        let at = at.map_or_else(String::new, |at| format!("dir = \"{}\"\n", at.display()));
        format!(
            "[[account]]\nlabel = \"{label}\"\nclass = \"dedicated\"\n{at}email =              \"{label}@example.com\"\n\n"
        )
    };
    let text = [
        row("home", None),
        row("made", Some(&made)),
        row("bare", Some(&dir.join("bare"))),
    ]
    .concat();
    std::fs::write(&roster, text).expect("roster");
    roster
}

/// THE COVERED ROWS SAY WHICH LOGINS THIS MAC HOLDS: a row's own directory that holds a login,
/// or a session's login record that names its account, the default login's among them. A row
/// on the default login has no directory and always counts as enrolled, so with no login record
/// naming its account it holds none, and neither does a row whose directory is empty: both read
/// `ok` by their records alone. CONTROL: the row whose directory holds a login reads `ok` with no
/// session at all, and each of the other two reads `ok`, logged in, once a session's record
/// names its account, so its offer's pair still says `ok`.
#[test]
fn the_covered_rows_say_which_logins_this_mac_holds() {
    let (dir, store) = scratch();
    let roster = the_roster_of_three(dir.path());
    let of = |logins: &[SessionLogin], label: &str| {
        let rows = journal::with_keyer(keyed_by_email, || {
            super::covered(&Places {
                store: &store,
                roster: Some(&roster),
                logins,
                now: i64::try_from(NOW).unwrap(),
                offers: true,
                enrolled: &accounts::enrolled,
            })
        });
        assert_eq!(rows.len(), 3, "{rows:?}");
        let row = rows
            .into_iter()
            .find(|row| row.label.as_deref() == Some(label))
            .expect("the row is covered");
        (row.login, row.logged_in)
    };
    let held = LoginHere::Half(LoginHalf::Ok);
    assert_eq!(of(&[], "home"), (LoginHere::Absent, false));
    assert_eq!(of(&[], "bare"), (LoginHere::Absent, false));
    assert_eq!(of(&[], "made"), (held, false), "CONTROL: its own directory");
    for label in ["home", "bare"] {
        let on = [SessionLogin {
            email: format!("{label}@example.com"),
            account_id: None,
            default: true,
        }];
        assert_eq!(of(&on, label), (held, true), "CONTROL: {label}'s session");
    }
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
            _: u64,
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
                        enrolled: &accounts::enrolled,
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

/// WHILE A REPLACEMENT IS ON ITS WAY, THE VIEW JUDGES THE VAULT'S TOKEN BY ITS OWN FACTS (§5.6):
/// the vault holds a token made here whose windows were found to differ here, or that was refused
/// here (its standing's `refused_at`); a `--replace` of it waits in this Mac's vault writes, so the
/// table's row for the account is the waiting token's and the vault's token's facts are its kept
/// row. Every surface's read
/// says what the route, the verb and execution admission say ([`tokens::Table::row_for`]): the
/// token's own state, not in use (so its login half is not taken for one on the token), not yet
/// in vault, and no offer while the write waits. NEGATIVE CONTROL (applied to [`view`], then
/// reverted): judged by the waiting token's row, `table.rows.get(..)`, it reads `unverified
/// (awaiting a stored-login reading)` and in use.
#[test]
fn a_waiting_replacement_leaves_the_view_judging_the_vaults_token_by_its_own_facts() {
    use crate::harness::accounts_tokens::Verdict;
    use crate::harness::accounts_tokens::tests::{Answer, FakeVault};
    let mut held = standing(NOW + 300 * DAY, &[]);
    held.minted_here = true;
    if let Some(meta) = held.meta.as_mut() {
        meta.verified = None;
        meta.minted_on = "m22".to_owned();
    }
    let differ = TokenRow {
        verified: None,
        verification: Verification {
            verdict: Some(Verdict::Differ),
            ..Verification::default()
        },
        ..row()
    };
    let refused = TokenRow {
        verified: None,
        ..row()
    };
    for (case, kept, refused_at, words) in [
        ("differ", differ, None, "unverified (windows differ)"),
        ("rejected", refused, Some(NOW - 60), "rejected"),
    ] {
        let (_dir, store) = scratch();
        tokens::update_table(&store, |table: &mut Table| {
            table.rows.insert(
                KEY.to_owned(),
                TokenRow {
                    minted_at: Some(NOW - DAY + 600),
                    vault: InVault::Waiting("it waits for the vault's new key".to_owned()),
                    verified: None,
                    used_on: UsedOn::default(),
                    ..row()
                },
            );
            table.replaced.insert(KEY.to_owned(), kept);
        })
        .expect("the table");
        let mut vault = FakeVault::new(Answer::Found(Box::new(Standing {
            refused_at,
            ..held.clone()
        })));
        vault.pending = true;
        let shown = journal::with_keyer(
            |_, _| Some(KEY.to_owned()),
            || {
                view(
                    &Places {
                        store: &store,
                        roster: None,
                        logins: &[],
                        now: i64::try_from(NOW).unwrap(),
                        offers: true,
                        enrolled: &accounts::enrolled,
                    },
                    &vault,
                )
            },
        );
        assert_eq!(shown.len(), 1, "{case}");
        assert_eq!(shown[0].state.words(), words, "{case}");
        assert!(!shown[0].state.in_use(), "{case}");
        assert_eq!(shown[0].login, LoginHere::Unread, "{case}: no token in use");
        assert!(
            shown[0].not_yet(),
            "{case}: the replacement is not yet in vault"
        );
        assert!(shown[0].offer.is_none(), "{case}: none while it waits");
    }
}

/// A vault whose post a server takes where `takes`, counting the posts it was asked for, and
/// keeping the keys each ask named (raising one need for each); it answers nothing else.
struct Posting {
    takes: bool,
    posts: RefCell<u32>,
    asked: RefCell<Vec<String>>,
}

impl Posting {
    fn new(takes: bool) -> Self {
        Self {
            takes,
            posts: RefCell::new(0),
            asked: RefCell::new(Vec::new()),
        }
    }
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
        _: u64,
        _: Verified,
        _: aterm_secrets::client::OnBusy,
    ) -> Result<aterm_secrets::client::MemberWritten, String> {
        panic!("no verification")
    }
    fn post(&self) -> Result<bool, String> {
        *self.posts.borrow_mut() += 1;
        Ok(self.takes)
    }
    fn ask(&self, keys: &[Key]) -> Result<usize, String> {
        self.asked
            .borrow_mut()
            .extend(keys.iter().map(|key| key.as_str().to_owned()));
        Ok(keys.len())
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
    let offline = Posting::new(false);
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

    let online = Posting::new(true);
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
        steps: Held::default(),
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

/// A STOPPED TOKEN STEP IS SHOWN ON DEMAND WHILE ITS RECORD HOLDS (FL-136): the report's line
/// says when, what stopped it and what tries it again (for a check, only a person's act), its
/// JSON carries the word and when, and neither names a host; a check's record stands through a
/// new Claude Code build and through a token made after it (another Mac's: it says nothing of
/// the page this Mac met), and a record of any other stop a later token released is not shown.
/// NEGATIVE CONTROL: with `held_steps` keeping every record, the account whose token was made
/// after its stop still shows it: `` a released record was shown ``; with `Stopped::holds`
/// reading a check's word as any other's, `` a new build released a check's record ``; with it
/// letting a later token end a check's hold (the merge's form), `` a later token released a
/// check's record ``.
#[test]
fn a_stopped_token_step_is_shown_on_demand_while_its_record_holds() {
    use crate::harness::open_url::MetWord;
    let (_dir, store) = scratch();
    let none = account(
        covered(LoginHalf::Ok),
        &Lookup::Absent,
        None,
        Some(false),
        NOW,
    );
    let step = Stopped::new(MetWord::WindowHumanCheck, NOW, "2.1.318", "").expect("a record");
    token_steps::put(&store, KEY, Some(&step)).unwrap();
    let steps = held_steps(
        &store,
        std::slice::from_ref(&none),
        (None, &|_| None),
        || Some("2.1.318".to_owned()),
    );
    assert_eq!(steps.stopped.get(KEY), Some(&step));
    assert!(steps.tried.is_empty(), "{steps:?}");
    let report = Report::Accounts {
        accounts: vec![none.clone()],
        peers: PeersShown::NotRead,
        sessions: None,
        steps,
    };
    let text = report.text();
    assert!(
        text.contains(
            "\n    token step: stopped 2027-01-15: the consent page held Authorize behind a \
             human check; it is tried again only by a person at a terminal on this Mac, who \
             meets the check in aterm's sign-in window: at their own sign-in of the account \
             there, or by `aterm harness accounts token work`"
        ),
        "{text}"
    );
    assert!(!text.contains("claude.ai"), "{text}");
    let json = report.json();
    assert_eq!(
        json["accounts"][0]["step"]["word"],
        Value::from("window-human-check")
    );
    assert_eq!(json["accounts"][0]["step"]["at"], Value::from(NOW));
    // A new Claude Code build does not release a check's record; a token made after the stop
    // does.
    assert_eq!(
        held_steps(
            &store,
            std::slice::from_ref(&none),
            (None, &|_| None),
            || { Some("2.1.319".to_owned()) }
        )
        .stopped
        .get(KEY),
        Some(&step),
        "a new build released a check's record"
    );
    let made = account(
        covered(LoginHalf::Ok),
        &Lookup::Found(standing(NOW + 300 * DAY, &[])),
        None,
        Some(false),
        NOW + 2 * DAY,
    );
    let mut later = step.clone();
    later.at = NOW - 2 * DAY;
    token_steps::put(&store, KEY, Some(&later)).unwrap();
    assert_eq!(
        held_steps(
            &store,
            std::slice::from_ref(&made),
            (None, &|_| None),
            || None
        )
        .stopped
        .get(KEY),
        Some(&later),
        "a later token released a check's record"
    );
    // The same token releases the record of a stop no check made.
    token_steps::put(&store, KEY, None).unwrap();
    let mut other =
        Stopped::new(MetWord::WindowWrongAccount, NOW, "2.1.318", "").expect("a record");
    other.at = NOW - 2 * DAY;
    token_steps::put(&store, KEY, Some(&other)).unwrap();
    assert!(
        held_steps(
            &store,
            std::slice::from_ref(&made),
            (None, &|_| None),
            || None
        )
        .is_empty(),
        "a released record was shown"
    );
}

/// THE UPKEEP'S RECORD OF A RUN IS SHOWN ON DEMAND WHILE IT HOLDS THE GAP (FL-138): a code
/// page's stop that a mailbox the window reads now let through is not shown, while the upkeep's
/// record of the run that met it, which no mailbox lets through, is: the report's line says when,
/// the gap and what runs it again, its JSON carries the gap, the token's `minted_at` and when,
/// and neither names a host. A record of another token's gap (the vault holds a token made
/// since), or one a new Claude Code build released, is not shown.
/// NEGATIVE CONTROL: with `held_steps` reading no tried record, the report says nothing of why
/// no token is being made: `` the upkeep's held run was not shown ``.
#[test]
fn the_upkeeps_record_of_a_run_is_shown_on_demand_while_it_holds_the_gap() {
    use crate::harness::accounts_tokens::Gap;
    use crate::harness::open_url::MetWord;
    let (_dir, store) = scratch();
    let none = account(
        covered(LoginHalf::Ok),
        &Lookup::Absent,
        None,
        Some(false),
        NOW,
    );
    let stop = Stopped::new(MetWord::WindowCodeNoMailbox, NOW, "2.1.318", "").expect("a record");
    token_steps::put(&store, KEY, Some(&stop)).unwrap();
    let ran = Tried::new((Gap::None, None), NOW, "2.1.318", &aterm_stamp::build_id());
    token_steps::put_tried(&store, KEY, Some(&ran)).unwrap();
    // aterm's window reads the account's mailbox now, from a configuration no file dates (as a
    // vault mailbox entry is one).
    let mailbox = |name: &str| -> Option<std::ffi::OsString> {
        use crate::harness::headless_oauth::{IMAP_PASS_ENV, IMAP_URL_ENV, IMAP_USER_ENV};
        match name {
            IMAP_URL_ENV => Some("imaps://imap.example.test/INBOX".into()),
            IMAP_USER_ENV => Some(EMAIL.into()),
            IMAP_PASS_ENV => Some("a fixture's word".into()),
            _ => None,
        }
    };
    let held = |accounts: &[AccountToken], cli: &str| {
        held_steps(&store, accounts, (None, &mailbox), || Some(cli.to_owned()))
    };
    let steps = held(std::slice::from_ref(&none), "2.1.318");
    assert!(
        steps.stopped.is_empty(),
        "CONTROL: the window's mailbox let the code page's stop through: {steps:?}"
    );
    assert_eq!(
        steps.tried.get(KEY),
        Some(&ran),
        "the upkeep's held run was not shown"
    );
    let report = Report::Accounts {
        accounts: vec![none.clone()],
        peers: PeersShown::NotRead,
        sessions: None,
        steps,
    };
    let text = report.text();
    assert!(
        text.contains(
            "\n    token upkeep: ran 2027-01-15 for its year-long token (none): this Mac's \
             upkeep's capture with nobody there made no token, and it runs none again for that \
             token until a new Claude Code or aterm build, the account's window store, a mailbox \
             configuration file or its saved Google mail grant written after it, or a token made \
             after it, such as `aterm harness accounts token work` makes"
        ),
        "{text}"
    );
    assert!(!text.contains("token step:"), "{text}");
    assert!(!text.contains("example.test"), "{text}");
    let json = report.json();
    assert_eq!(json["accounts"][0]["tried"]["gap"], Value::from("none"));
    assert_eq!(json["accounts"][0]["tried"]["minted_at"], Value::Null);
    assert_eq!(json["accounts"][0]["tried"]["at"], Value::from(NOW));
    assert_eq!(json["accounts"][0]["step"], Value::Null);

    assert!(
        held(std::slice::from_ref(&none), "2.1.319")
            .tried
            .is_empty(),
        "a new Claude Code build's run was held"
    );
    let made = account(
        covered(LoginHalf::Ok),
        &Lookup::Found(standing(NOW + 300 * DAY, &[])),
        None,
        Some(false),
        NOW,
    );
    assert!(
        held(std::slice::from_ref(&made), "2.1.318")
            .tried
            .is_empty(),
        "another token's gap was shown"
    );
}

/// THE LAB IS ASKED FOR AN OFFERED TOKEN NOTHING ANSWERS HERE, NEVER FOR ONE IN USE (§5.9): an
/// offer for an account with no token, an ended one or one Claude Code refused here is asked of
/// the lab under the account's key; an offer whose token is verified and in use (`soon`), which
/// that request would count as met at once, asks nothing, nor does an account with no offer (a
/// write of this Mac's still waits, or no session here is logged into it).
///
/// NEGATIVE CONTROL (2026-11-14, restored with cp from a backup plus touch, confirmed with cmp):
/// with `ask_lab`'s in-use filter removed, `soon` reads
/// `left: (Soon, true, ["ac-fedcba9876543210"])`, `right: (Soon, true, [])`.
#[test]
fn an_offer_with_no_token_in_use_asks_the_lab_and_one_in_use_asks_nothing() {
    let vault = Posting::new(false);
    let asked = |shown: AccountToken| {
        vault.asked.borrow_mut().clear();
        let raised = ask_lab(std::slice::from_ref(&shown), &vault, NOW).expect("asked");
        let asked = vault.asked.borrow().clone();
        assert_eq!(raised, asked.len());
        (shown.state, shown.offer.is_some(), asked)
    };
    let here = |found: Lookup<Standing>, pending: bool| {
        account(covered(LoginHalf::Ok), &found, None, Some(pending), NOW)
    };
    let mut refused = standing(NOW + 300 * DAY, &[]);
    refused.refused_at = Some(NOW - 60);
    // Made here at a terminal, never verified: it serves this Mac alone, so its refusal here
    // leaves it in use nowhere.
    let mut refused_alone = refused.clone();
    refused_alone.minted_here = true;
    if let Some(meta) = refused_alone.meta.as_mut() {
        meta.minted_on = "m2".to_owned();
        meta.minted_by = Some(aterm_secrets::claude_token::MintedBy::Terminal);
        meta.verified = None;
    }
    for (case, found, state) in [
        ("none", Lookup::Absent, TokenState::None),
        (
            "ended",
            Lookup::Found(standing(NOW - DAY / 2, &[])),
            TokenState::Ended,
        ),
        (
            "refused here, in use on other Macs",
            Lookup::Found(refused),
            TokenState::Rejected { elsewhere: true },
        ),
        (
            "refused here, the Mac that made it alone",
            Lookup::Found(refused_alone),
            TokenState::Rejected { elsewhere: false },
        ),
    ] {
        assert_eq!(
            asked(here(found, false)),
            (state, true, vec![KEY.to_owned()]),
            "{case}"
        );
    }
    assert_eq!(
        asked(here(Lookup::Found(standing(NOW + 10 * DAY, &[])), false)),
        (TokenState::Soon, true, Vec::new()),
        "soon: offered, and in use"
    );
    assert_eq!(
        asked(here(Lookup::Absent, true)),
        (TokenState::None, false, Vec::new()),
        "a write of this Mac's waits"
    );
    let mut away = covered(LoginHalf::Ok);
    away.logged_in = false;
    assert_eq!(
        asked(account(away, &Lookup::Absent, None, Some(false), NOW)),
        (TokenState::None, false, Vec::new()),
        "no session here is logged into it"
    );
}

/// What the surfaces say of the account a read at `now` finds: the state's words and word, the
/// login half's word, the state line's phrase, the offer's pair and step and the Lab page's line.
fn shown_at(account: &AccountToken, now: i64) -> String {
    format!(
        "{} | {} | {} | {:?} | {:?} | {:?}",
        account.state.word(),
        account.state.words(),
        account.login.word(),
        account.phrase(now),
        account
            .offer
            .as_ref()
            .map(|offer| (offer.pair.clone(), offer.step.to_string())),
        account.offer_line(),
    )
}

/// The law's sweep over one account whose read at `t` is `read(t)`, from `from` to `to`, every
/// `step` seconds and at each second a read named and the one before it: wherever two
/// consecutive instants show the account otherwise, the earlier one's read named a second no
/// later than the later one ([`AccountToken::changes_after`]). Returns how many changes it saw.
fn sweep(
    name: &str,
    (from, to): (i64, i64),
    step: i64,
    read: impl Fn(i64) -> AccountToken,
) -> usize {
    let mut at: std::collections::BTreeSet<i64> = (from..to).step_by(step as usize).collect();
    for t in at.clone() {
        if let Some(due) = read(t).changes_after(t).filter(|due| *due < to) {
            at.extend([due - 1, due]);
        }
    }
    let at: Vec<i64> = at.into_iter().filter(|t| (from..to).contains(t)).collect();
    let mut changes = 0;
    for pair in at.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (was, then) = (read(a), read(b));
        if shown_at(&was, a) != shown_at(&then, b) {
            changes += 1;
            let due = was.changes_after(a);
            assert!(
                due.is_some_and(|due| due <= b),
                "{name}: read at {a} named {due:?}, but at {b} it shows {:?} where it showed {:?}",
                shown_at(&then, b),
                shown_at(&was, a),
            );
        }
    }
    changes
}

/// A TOKEN VIEW'S DEADLINE COMES NO LATER THAN THE FIRST CHANGE THE CLOCK ALONE MAKES
/// ([`AccountToken::changes_after`], the token lane's computed deadline): over a month of a
/// verified token (`active`, `soon` 30 days before its end, `ended` at it), ten days of an ending
/// login on no token (the offer pair's half `ok`, then `ending` with the state line counting the
/// days down, then `ended`), and an unverified token whose basis's week ends (`no reading yet`,
/// then `awaiting a stored-login reading`), every change of what the surfaces say comes at or
/// after the second the read before it named, each read made as a read then would make it.
///
/// NEGATIVE CONTROLS (run 2026-11-14): with the `soon` term left out of `changes_after`, the
/// verified token's sweep fails where `active` turns `soon` (`a verified token: read at …
/// named Some(<the end>)`); with the state line's days left out, the ending login's fails where
/// `login ends in 6 days` turns 5 (`an ending login: read at … named Some(<the end>)`).
#[test]
fn the_views_deadline_is_no_later_than_its_first_change() {
    let day = i64::try_from(DAY).unwrap();
    let now = i64::try_from(NOW).unwrap();
    let end = NOW + 31 * DAY;
    let verified = Lookup::Found(standing(end, &[]));
    let read = |t: i64| {
        account(
            covered(LoginHalf::Ok),
            &verified,
            None,
            Some(false),
            u64::try_from(t).unwrap(),
        )
    };
    assert_eq!(
        sweep("a verified token", (now, now + 32 * day), 3_600, read),
        2,
        "active, soon, ended"
    );

    let login_end = now + 10 * day;
    let read = |t: i64| {
        let covered = Covered {
            login: LoginHere::Half(LoginHalf::of_forecast(Some(login_end), t)),
            login_turns: LoginHalf::forecast_turns(Some(login_end), t),
            ..covered(LoginHalf::Ok)
        };
        account(
            covered,
            &Lookup::Absent,
            None,
            Some(false),
            u64::try_from(t).unwrap(),
        )
    };
    assert!(
        read(now).offer.is_some(),
        "an offer stands for a login on no token"
    );
    assert_eq!(
        sweep("an ending login", (now, now + 11 * day), 3_600, read),
        8,
        "ok, ending in 6 days to today, ended"
    );

    let mut unverified = standing(NOW + 300 * DAY, &[]);
    if let Some(meta) = unverified.meta.as_mut() {
        meta.verified = None;
    }
    unverified.minted_here = true;
    let minted_at = unverified.meta.as_ref().map(|meta| meta.minted_at);
    let week = now + 2 * day;
    let kept = TokenRow {
        minted_at,
        verified: None,
        verification: Verification {
            basis: Some((
                NOW - DAY,
                crate::harness::accounts_tokens::Windows {
                    week_reset: Some(week),
                    session_reset: None,
                },
            )),
            reading: None,
            verdict: None,
            shared_with: std::collections::BTreeSet::new(),
        },
        ..row()
    };
    let found = Lookup::Found(unverified);
    let read = |t: i64| {
        account(
            covered(LoginHalf::Ok),
            &found,
            Some(kept.clone()),
            Some(false),
            u64::try_from(t).unwrap(),
        )
    };
    assert!(
        matches!(
            read(now).state,
            TokenState::Unverified {
                why: tokens::Unverified::NoReadingYet,
                ..
            }
        ),
        "{:?}",
        read(now).state
    );
    assert_eq!(
        sweep(
            "an unverified token's basis",
            (now, now + 3 * day),
            3_600,
            read
        ),
        1,
        "no reading yet, awaiting a stored-login reading"
    );
}

/// A READ SET NAMES THE FILES THE VIEW OPENS, AND ITS STAMP MOVES ONLY WITH THEM
/// ([`read_set_here`], [`ReadSet::stamp`]): the accounts store's state, the table, the steps and
/// the upkeep's tried gaps ([`held_steps`] reads both), the roster, the default login's record
/// and the vault's source are among its files; stamped twice with nothing written between, it
/// stands; a table written in place of the one stamped moves it, and so does a vault configured
/// under the home after it was stamped.
///
/// NEGATIVE CONTROLS: with the table left out of `read_set_here`'s files (run 2026-11-14), the
/// test fails at `…/accounts/tokens.json not in [`; with the tried gaps left out (run
/// 2026-11-16), at `…/accounts/tokens-tried.json not in [`.
#[test]
fn a_read_set_names_the_views_files_and_its_stamp_moves_with_them() {
    let (dir, store) = scratch();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let config = dir.path().join("config").join("aterm.toml");
    let roster = accounts::roster_beside_for(&config, Cli::Claude).expect("a roster beside");
    let now = i64::try_from(NOW).unwrap();
    let set = read_set_here(dir.path(), Some(&config), Some(&home), now);
    let paths = set.paths();
    for want in [
        store.state_file(),
        store.shared_dir().join(tokens::TABLE_FILE),
        store.shared_dir().join(token_steps::STEPS_FILE),
        store.shared_dir().join(token_steps::TRIED_FILE),
        roster.clone(),
        crate::harness::profile::login_record_path(&home, None).expect("the default login's"),
        aterm_secrets::client::Paths::locate_for_home(&home)
            .dir
            .join("source.toml"),
    ] {
        assert!(
            paths.contains(&want),
            "{} not in {paths:#?}",
            want.display()
        );
    }
    let before = set.stamp();
    assert!(set.stamp() == before, "nothing written, the stamp stands");
    tokens::update_table(&store, |table| {
        table.rows.insert(KEY.to_owned(), row());
    })
    .expect("the table is written");
    assert!(set.stamp() != before, "a table write moves the stamp");
    let written = set.stamp();
    let vault = aterm_secrets::client::Paths::locate_for_home(&home);
    std::fs::create_dir_all(&vault.dir).unwrap();
    std::fs::write(vault.dir.join("source.toml"), "fixture = true\n").unwrap();
    assert!(set.stamp() != written, "a vault made later moves the stamp");
}

/// A HELD CODE PAGE'S READ SET NAMES THE MAILBOX CONFIGURATIONS ITS JUDGE READS
/// ([`read_set_here`], [`token_steps::mailbox_paths`]): with no step waiting on a mailbox the set
/// names no `aterm.toml` under the home; with a code page's stop held
/// ([`token_steps::Stopped::waits_on_a_mailbox`]) it names the home's `aterm.toml` and
/// `mailbox.toml`, another agent's roster, the fleet roster and the default mail folder there,
/// and a mailbox written into that `aterm.toml`, the write that lets the stop through
/// ([`held_steps`], the CONTROL that the file is one its judge reads), moves the set's stamp.
///
/// NEGATIVE CONTROL (run 2026-11-16, restored): with `read_set_here` naming no
/// [`token_steps::mailbox_paths`], the test fails at `…/home/.config/aterm/aterm.toml not in [`.
#[test]
fn a_held_code_pages_read_set_names_the_mailbox_configurations_its_judge_reads() {
    use crate::harness::open_url::MetWord;
    let (dir, store) = scratch();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let config = dir.path().join("config").join("aterm.toml");
    let now = i64::try_from(NOW).unwrap();
    let config_dir = home.join(".config/aterm");
    let aterm_toml = config_dir.join("aterm.toml");
    let unheld = read_set_here(dir.path(), Some(&config), Some(&home), now).paths();
    assert!(
        !unheld.contains(&aterm_toml),
        "no step waits on a mailbox, and no aterm.toml is named: {unheld:#?}"
    );
    let stop = Stopped::new(MetWord::WindowCodeNoMailbox, NOW, "2.1.318", "").expect("a record");
    token_steps::put(&store, KEY, Some(&stop)).unwrap();
    let set = read_set_here(dir.path(), Some(&config), Some(&home), now);
    let paths = set.paths();
    for want in [
        aterm_toml.clone(),
        config_dir.join(crate::harness::headless_oauth::MAILBOX_TOML_FILE),
        config_dir.join(Cli::Codex.accounts().roster_file),
        config_dir.join(crate::harness::accounts_sync::FLEET_ROSTER_FILE),
        config_dir.join(crate::harness::open_url::DEFAULT_MAILBOX_DIR),
    ] {
        assert!(
            paths.contains(&want),
            "{} not in {paths:#?}",
            want.display()
        );
    }
    let none = account(
        covered(LoginHalf::Ok),
        &Lookup::Absent,
        None,
        Some(false),
        NOW,
    );
    let held = || {
        held_steps(
            &store,
            std::slice::from_ref(&none),
            (Some(&home), &|_| None),
            || Some("2.1.318".to_owned()),
        )
        .stopped
    };
    assert!(
        held().contains_key(KEY),
        "CONTROL: no mailbox is read, and the code page's stop holds"
    );
    let before = set.stamp();
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        &aterm_toml,
        format!(
            "[mailbox]\nimap_url = \"imaps://imap.example.test/INBOX\"\nuser = \"{EMAIL}\"\n\
             password = \"a fixture's word\"\n"
        ),
    )
    .unwrap();
    assert!(
        held().is_empty(),
        "CONTROL: the window reads the mailbox aterm.toml configures, and the stop is let through"
    );
    assert!(
        set.stamp() != before,
        "a mailbox written into aterm.toml moves the stamp"
    );
}
