// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ONE RULE FOR A LAPSED LOGIN (one-mesh AC1; E4's record
//! `docs/measured/e2e-local-signins-2026-11-05.md`, "Left open"). A login whose own credential
//! file here shows it lapsed is signed out here ([`signed_out_here`]), so [`login_state`] reads it
//! [`LoginState::Expired`] exactly where [`condition`] reads it [`Condition::SignedOut`], whatever
//! its records forecast: local state, the CLI's own credential file included, answers whether an
//! account is signed in here (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.15 rule 6). A row with
//! a token on file reads as its records say. The upkeep's doors, the renewal door
//! ([`renewal_due`], §4.15 rule 4) and the session door, open on the records alone, so a lapse
//! starts no sign-in by itself (the owner's rule for AC1). Each pool lays real credential files
//! and reads their lapse as every reader does ([`Store::hydrate_lapsed`]).

use super::*;

const NOW: i64 = 1_790_000_000;
const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;

/// A lapsed Claude Code login: no token (`aterm-lab`'s `claude_login_proof::DEAD_LOGIN`).
const CLAUDE_DEAD: &str = "{\"claudeAiOauth\":{\"accessToken\":\"\",\"refreshToken\":\"\",\
                           \"expiresAt\":0,\"scopes\":[\"user:inference\",\"user:profile\"],\
                           \"subscriptionType\":null}}\n";
/// A live Claude Code login: both tokens, expiring far ahead.
const CLAUDE_LIVE: &str = "{\"claudeAiOauth\":{\"accessToken\":\"synthetic-access\",\
                           \"refreshToken\":\"synthetic-refresh\",\"expiresAt\":4102444800000}}\n";
/// A Claude Code login whose access token expired and whose refresh token renews it.
const CLAUDE_REFRESHABLE: &str = "{\"claudeAiOauth\":{\"accessToken\":\"\",\
                                  \"refreshToken\":\"synthetic-refresh\",\"expiresAt\":0}}\n";
/// A Grok Build login file; [`Shape::refused`] marks it refused after, which lapses it.
const GROK_LOGIN: &str = "{\"synthetic\":\"grok login\"}\n";

/// One row's login: its credential file's bytes, and whether aterm marked it refused after.
struct Shape {
    label: &'static str,
    body: &'static str,
    refused: bool,
}

const fn shape(label: &'static str, body: &'static str) -> Shape {
    Shape {
        label,
        body,
        refused: false,
    }
}

/// What a row's records say at [`NOW`], beyond a renewal into its own directory and a session
/// here that ran on it then ([`used_here`]).
#[derive(Clone, Copy, Debug)]
enum Records {
    /// Nothing more: neither served lately nor forecast to end soon.
    Idle,
    /// A turn served under it at [`NOW`].
    Ok,
    /// A notice that it ends a day after [`NOW`].
    Due,
    /// A wall a session met at [`NOW`] ([`note_login_end`]).
    Ended,
}

impl Records {
    const ALL: [Records; 4] = [Records::Idle, Records::Ok, Records::Due, Records::Ended];

    /// What [`login_state`] answered of a row before AC1, by its records alone.
    const fn by_records(self) -> LoginState {
        match self {
            Records::Idle => LoginState::Idle,
            Records::Ok => LoginState::Ok,
            Records::Due => LoginState::Due { by: NOW + DAY },
            Records::Ended => LoginState::Expired,
        }
    }
}

/// `cli`'s pool of `shapes`, each row naming an email and a directory of its own that holds its
/// credential file.
struct Pool {
    _dir: aterm_tempfile::TempDir,
    cli: Cli,
    store: Store,
    roster: Roster,
}

fn pool(cli: Cli, shapes: &[Shape]) -> Pool {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-lapsed-rule-")
        .tempdir()
        .expect("temp dir");
    let home = dir.path().join("home");
    let store = Store::at_for(&dir.path().join("state"), cli, &home);
    let rows = shapes
        .iter()
        .map(|shape| {
            let login = dir.path().join("logins").join(shape.label);
            let credential = login.join(cli.connector().stores[0]);
            std::fs::create_dir_all(credential.parent().expect("a folder")).expect("mkdir");
            std::fs::write(&credential, shape.body).expect("write the credential");
            if shape.refused {
                crate::harness::grok_signin::mark_refused(&credential).expect("mark it refused");
            }
            Row {
                label: shape.label.into(),
                dir: Some(login.to_str().expect("a UTF-8 path").to_owned()),
                email: Some(format!("{}@example.test", shape.label)),
                ..Row::default()
            }
        })
        .collect();
    Pool {
        _dir: dir,
        cli,
        store,
        roster: Roster { rows },
    }
}

impl Pool {
    /// Every row renewed into its own directory and run on here an hour before [`NOW`], then
    /// `records` at [`NOW`]: the bound roster, the state with the lapse its files show hydrated
    /// as [`Store::read_account_records`] hydrates it, the logins, and the enrolled set.
    fn judged(&self, records: Records) -> (Roster, State, Logins, BTreeSet<String>) {
        let mut state = State {
            machine: "0a0b".to_owned(),
            ..State::default()
        };
        let mut logins = Logins::default();
        let at = NOW - HOUR;
        for row in &self.roster.rows {
            let dir = row.dir.as_deref().expect("a directory");
            let _ = note_renewed(
                &mut state,
                &mut logins,
                &row.label,
                Some(dir),
                dir,
                Some("max"),
                at,
            );
            let _ = note_session(
                &mut state,
                &format!("s-{}", row.label),
                &row.label,
                None,
                at,
            );
            match records {
                Records::Idle => {}
                Records::Ok => {
                    let _ = note_served_login(&mut logins, &row.label, NOW);
                }
                Records::Due => {
                    let _ = note_login_notice(&mut logins, &row.label, 1, NOW);
                }
                Records::Ended => {
                    let _ = note_login_end(&mut state, &mut logins, &row.label, NOW);
                }
            }
        }
        // The sessions have ended: only their runs stay on record.
        state.sessions.clear();
        let roster = bound(&self.roster, &logins);
        self.store.hydrate_lapsed(&mut state, &roster);
        let enrolled = enrolled_for(self.cli, &roster);
        (roster, state, logins, enrolled)
    }
}

/// A LAPSED FILE LOGIN READS EXPIRED WHERE THE REPORT READS IT SIGNED OUT, whatever its records
/// forecast: a tokenless Claude Code login with an email, idle, served lately or due. CONTROL:
/// its records alone ([`recorded_login_state`], `login_state` before AC1) answer idle, ok or due,
/// the two readings E4 found.
#[test]
fn a_lapsed_file_login_reads_expired_in_login_state_where_condition_reads_it_signed_out() {
    let pool = pool(Cli::Claude, &[shape("dead", CLAUDE_DEAD)]);
    for records in [Records::Idle, Records::Ok, Records::Due] {
        let (roster, state, logins, enrolled) = pool.judged(records);
        let dead = roster.row("dead").expect("the row");
        assert!(enrolled.contains("dead"), "its login is made here");
        assert!(
            lapsed_here(dead, &state),
            "{records:?}: its file shows it lapsed"
        );
        assert!(signed_out_here(dead, &state), "{records:?}");
        assert_eq!(
            condition(dead, &state, true, NOW),
            Condition::SignedOut,
            "{records:?}"
        );
        assert_eq!(
            login_state(dead, &logins, &state, NOW),
            LoginState::Expired,
            "{records:?}: the one rule, as `condition` reads it"
        );
        assert_eq!(
            recorded_login_state(dead, &logins, &state, NOW),
            records.by_records(),
            "{records:?}: CONTROL, the records alone read it otherwise"
        );
    }
}

/// A ROW WITH A TOKEN ON FILE IS UNCHANGED: a live Claude Code login and a refreshable one (an
/// expired access token beside a refresh token, which the CLI renews) read as their records
/// say, signed out only where a wall was recorded.
#[test]
fn a_row_with_a_token_on_file_reads_as_its_records_say() {
    let pool = pool(
        Cli::Claude,
        &[
            shape("live", CLAUDE_LIVE),
            shape("refresh", CLAUDE_REFRESHABLE),
        ],
    );
    for records in Records::ALL {
        let (roster, state, logins, _) = pool.judged(records);
        for row in &roster.rows {
            assert!(
                !lapsed_here(row, &state),
                "{}: a token is on file",
                row.label
            );
            assert_eq!(
                login_state(row, &logins, &state, NOW),
                records.by_records(),
                "{}: {records:?}",
                row.label
            );
            assert_eq!(
                login_state(row, &logins, &state, NOW),
                recorded_login_state(row, &logins, &state, NOW),
                "{}: {records:?}",
                row.label
            );
            assert_eq!(
                condition(row, &state, true, NOW) == Condition::SignedOut,
                matches!(records, Records::Ended),
                "{}: {records:?}",
                row.label
            );
        }
    }
}

/// ONE RULE, BOTH VIEWS: over Claude Code's and Grok Build's lapses (a tokenless file, a login
/// aterm marked refused), live logins beside them, and every kind of record, an enrolled row reads
/// signed out in [`condition`] exactly where [`login_state`] reads it expired.
#[test]
fn condition_reads_signed_out_exactly_where_login_state_reads_expired() {
    let pools = [
        pool(
            Cli::Claude,
            &[
                shape("dead", CLAUDE_DEAD),
                shape("live", CLAUDE_LIVE),
                shape("refresh", CLAUDE_REFRESHABLE),
            ],
        ),
        pool(
            Cli::Grok,
            &[
                Shape {
                    label: "refused",
                    body: GROK_LOGIN,
                    refused: true,
                },
                shape("accepted", GROK_LOGIN),
            ],
        ),
    ];
    let mut lapsed = BTreeSet::new();
    for pool in &pools {
        for records in Records::ALL {
            let (roster, state, logins, enrolled) = pool.judged(records);
            for row in &roster.rows {
                assert!(enrolled.contains(&row.label), "{}", row.label);
                if lapsed_here(row, &state) {
                    lapsed.insert(row.label.clone());
                }
                assert_eq!(
                    condition(row, &state, true, NOW) == Condition::SignedOut,
                    login_state(row, &logins, &state, NOW) == LoginState::Expired,
                    "{}: {records:?}",
                    row.label
                );
            }
        }
    }
    assert_eq!(
        lapsed,
        BTreeSet::from(["dead".to_owned(), "refused".to_owned()]),
        "each pool holds one lapse its files show"
    );
}

/// THE RENEWAL DOOR OPENS ON THE RECORDS, NEVER ON A LAPSED FILE: a lapsed login this Mac uses,
/// idle or served lately on record, is no renewal and its report says why; a forecast end or a
/// recorded wall still opens the door for it, as for any login (§4.15 rule 4: a lapse takes no
/// renewal away). NEGATIVE CONTROL: with the door reading `login_state`
/// ([`with_renewal_on_login_state`]), the lapse alone would open it.
#[test]
fn the_renewal_door_opens_on_the_records_never_on_a_lapsed_file() {
    let pool = pool(
        Cli::Claude,
        &[shape("dead", CLAUDE_DEAD), shape("live", CLAUDE_LIVE)],
    );
    for records in [Records::Idle, Records::Ok] {
        let (roster, state, logins, enrolled) = pool.judged(records);
        let dead = roster.row("dead").expect("the row");
        assert!(used_here(&state, "dead", NOW), "a session here ran on it");
        assert_eq!(login_state(dead, &logins, &state, NOW), LoginState::Expired);
        assert!(
            renew_due(&roster, &logins, &state, &OtherMacs::NONE, NOW).is_empty(),
            "{records:?}: a lapse opens no renewal"
        );
        assert!(due_logins_here(&roster, &logins, &state, &enrolled, NOW).is_empty());
        let together = (&logins, &state, &OtherMacs::NONE);
        assert!(!super::super::sign_in_ladder::upkeep_signs_in(
            dead, together, NOW
        ));
        assert_eq!(
            super::super::sign_in_ladder::upkeep_waits_because(dead, &state, &OtherMacs::NONE, NOW),
            "its credential file here shows its login lapsed, and a file alone never starts \
             aterm's own sign-in"
        );
        let live = roster.row("live").expect("the row");
        assert_eq!(
            super::super::sign_in_ladder::upkeep_waits_because(live, &state, &OtherMacs::NONE, NOW),
            "its login is not on record as ended or due here"
        );
        assert_eq!(
            with_renewal_on_login_state(|| renew_due(
                &roster,
                &logins,
                &state,
                &OtherMacs::NONE,
                NOW
            )),
            ["dead"],
            "{records:?}: NEGATIVE CONTROL, a door on `login_state` opens on the lapse"
        );
    }
    for records in [Records::Due, Records::Ended] {
        let (roster, state, logins, _) = pool.judged(records);
        assert_eq!(
            renew_due(&roster, &logins, &state, &OtherMacs::NONE, NOW),
            ["dead", "live"],
            "{records:?}: the records open the door, lapsed or not"
        );
    }
}

/// THE SESSION DOOR OPENS ON THE RECORD, NEVER ON A LAPSED FILE: a session here that runs on a
/// lapsed login whose records say nothing ended starts no sign-in by itself, whatever else its
/// records say. CONTROL: once a wall it met is on record, the same door admits the row (§4.15
/// rule 4: a session here runs on it and loses its login).
#[test]
fn the_session_door_opens_on_the_record_never_on_a_lapsed_file() {
    use super::super::sign_in_ladder::{SignIn, session_signed_out, upkeep_signs_in};
    let pool = pool(Cli::Claude, &[shape("dead", CLAUDE_DEAD)]);
    for (records, admits) in [
        (Records::Idle, false),
        (Records::Ok, false),
        (Records::Due, false),
        (Records::Ended, true),
    ] {
        let (roster, mut state, logins, _) = pool.judged(records);
        let _ = note_session(&mut state, "s-live", "dead", None, NOW);
        let dead = roster.row("dead").expect("the row");
        assert!(signed_out_here(dead, &state), "{records:?}");
        assert_eq!(session_signed_out(dead, &state), admits, "{records:?}");
        assert_eq!(
            SignIn::session_sign_in(Cli::Claude, dead, &state).is_some(),
            admits,
            "{records:?}"
        );
        if matches!(records, Records::Idle | Records::Ok) {
            assert!(
                !upkeep_signs_in(dead, (&logins, &state, &OtherMacs::NONE), NOW),
                "{records:?}: neither door the upkeep holds admits the lapse"
            );
        }
    }
}

/// A LAPSED LOGIN IS NOT A WORKING ONE: where every renewable row enrolled here is lapsed, none
/// remains working ([`none_working_within`]), as the report's "none lasts past tomorrow" warns.
/// CONTROL: the same row with a live login on file is working.
#[test]
fn a_lapsed_login_is_not_a_working_one() {
    for (body, none_working) in [(CLAUDE_DEAD, true), (CLAUDE_LIVE, false)] {
        let pool = pool(Cli::Claude, &[shape("only", body)]);
        let (roster, state, logins, enrolled) = pool.judged(Records::Idle);
        assert_eq!(
            none_working_within(&roster, &logins, &state, &enrolled, NOW, NONE_SOON_S),
            none_working,
            "{body}"
        );
    }
}

/// RENEWAL IS OFF FOR A ROW ON ITS VAULT TOKEN (`docs/DESIGN-claude-setup-token-2026-11-05.md`
/// §5.8): a row whose records open both doors (a due or ended login a session here runs on) is
/// renewed by no door once it runs on its vault token here ([`OtherMacs::on_token`]): the renewal
/// door, the session door the upkeep holds, the upkeep's ranked candidates (every tier) and a
/// person's `--due` sweep pass it by, and the report names why. CONTROL: the same records with no
/// token row open the doors, and the row stays a candidate.
#[test]
fn no_door_renews_a_row_on_its_vault_token() {
    use super::super::sign_in_ladder::{upkeep_signs_in, upkeep_waits_because};
    let pool = pool(
        Cli::Claude,
        &[shape("dead", CLAUDE_DEAD), shape("live", CLAUDE_LIVE)],
    );
    let token = OtherMacs {
        on_token: [("dead".to_owned(), u64::try_from(NOW + 300 * DAY).unwrap())].into(),
        ..OtherMacs::NONE
    };
    for records in [Records::Due, Records::Ended] {
        let (roster, mut state, logins, _) = pool.judged(records);
        let _ = note_session(&mut state, "s-live", "dead", None, NOW);
        let dead = roster.row("dead").expect("the row");
        let candidates = |others: &OtherMacs| -> Vec<String> {
            super::super::accounts_sync::needed_renew_candidates(
                &roster,
                &state,
                (&logins, others),
                NOW,
            )
            .into_iter()
            .map(|row| row.label)
            .collect()
        };
        assert!(
            renew_due(&roster, &logins, &state, &OtherMacs::NONE, NOW).contains(&"dead".to_owned()),
            "{records:?}: CONTROL, the records open the renewal door"
        );
        assert!(
            upkeep_signs_in(dead, (&logins, &state, &OtherMacs::NONE), NOW),
            "{records:?}: CONTROL"
        );
        assert!(
            candidates(&OtherMacs::NONE).contains(&"dead".to_owned()),
            "{records:?}: CONTROL, it is a candidate"
        );

        assert!(
            !renewal_due(dead, &logins, &state, &token, NOW),
            "{records:?}"
        );
        assert!(
            !renew_due(&roster, &logins, &state, &token, NOW).contains(&"dead".to_owned()),
            "{records:?}"
        );
        assert!(
            !upkeep_signs_in(dead, (&logins, &state, &token), NOW),
            "{records:?}"
        );
        assert!(
            !candidates(&token).contains(&"dead".to_owned()),
            "{records:?}: auto-renewal never picks it"
        );
        assert_eq!(
            upkeep_waits_because(dead, &state, &token, NOW),
            "it runs on its vault token here, and renewal is off for it"
        );
        assert!(
            renew_due(&roster, &logins, &state, &token, NOW).contains(&"live".to_owned()),
            "{records:?}: another row renews as before"
        );
    }
}
