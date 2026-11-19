// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the login verdict's fold over dated evidence (plan package UB-04).

use super::super::*;
use super::WITNESS_MAX;
use crate::harness::host_path::abs;
use aterm_evidence::Source;

const NOW: i64 = 1_790_000_000;
const HOUR: i64 = 3600;

fn roster() -> Roster {
    Roster::parse(
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"owner@example.com\"\n",
    )
    .expect("admits")
}

/// Row `main`'s login renewed at `at` and used here.
fn renewed(at: i64) -> (State, Logins) {
    let mut state = State {
        machine: "0a0b".to_string(),
        ..State::default()
    };
    let mut logins = Logins::default();
    note_renewed(
        &mut state,
        &mut logins,
        "main",
        None,
        &abs("/s/main.1"),
        Some("max"),
        at,
    );
    let _ = note_session(&mut state, "s-main", "main", None, at);
    state.sessions.clear();
    (state, logins)
}

fn obs(at: i64, witness: &str) -> LoginObs {
    LoginObs::new(at, witness, Source::Reading)
}

/// A TURN SERVED AFTER A STRIKE READS SIGNED IN WITH NOTHING WRITTEN TO LIFT IT: the wall's strike
/// stays on record as it was (its reading, the login's end), and the served turn is one more
/// reading, newer, which the verdict reads ([`Account::login_verdict`], [`signed_out_here`]); the
/// word on record is its projection, and the login's renewal, notice and learned lifetime stand,
/// as no credential changed. A turn served before the strike says nothing after it, the verdict
/// survives `state.json`, and a wall newer than the turn strikes again.
#[test]
fn a_turn_served_after_a_strike_reads_signed_in_with_nothing_written() {
    let r = roster();
    let row = r.row("main").expect("main");
    let (mut state, mut logins) = renewed(NOW);
    assert!(note_login_notice(&mut logins, "main", 3, NOW + HOUR));
    let struck = NOW + 2 * HOUR;
    let ended = note_login_end(&mut state, &mut logins, "main", struck);
    assert!(
        ended.iter().any(|e| e.kind == Kind::LoginEnded),
        "{ended:?}"
    );
    assert!(signed_out_here(row, &state), "PRECONDITION: struck");
    assert!(
        observe_served(&mut state, &mut logins, "main", struck - 60).is_empty(),
        "a turn served before the strike decides nothing"
    );
    assert!(signed_out_here(row, &state));
    let strike = state.account("main").login.struck.clone();
    assert_eq!(
        strike.as_ref().map(|obs| obs.at),
        Some(struck),
        "the strike is dated by its wall"
    );
    let before = logins.get("main").cloned().expect("bound");
    let served = struck + HOUR;
    let events = observe_served(&mut state, &mut logins, "main", served);
    assert_eq!(
        events
            .iter()
            .map(|e| (e.kind, e.why.as_str()))
            .collect::<Vec<_>>(),
        [(Kind::Auth, "ok")]
    );
    assert!(!signed_out_here(row, &state), "the newer turn decides");
    let account = state.account("main");
    let verdict = account.login_verdict(row).expect("a verdict");
    assert_eq!(
        (verdict.login, verdict.said, verdict.obs.at),
        (Login::Serves, Said::Served, served)
    );
    assert_eq!(account.login.struck, strike, "the strike stands on record");
    assert_eq!(
        (account.auth, account.auth_at),
        (Auth::Ok, Some(served)),
        "the word is the verdict's projection"
    );
    let binding = logins.get("main").expect("bound");
    assert_eq!(
        (
            binding.renewed_at,
            binding.notice_end,
            binding.lifetime(),
            binding.ended_at,
            binding.served_at,
        ),
        (
            before.renewed_at,
            before.notice_end,
            before.lifetime(),
            Some(struck),
            Some(served),
        ),
        "no sign-in: the renewal, notice, end and lifetime stand"
    );
    let home = aterm_tempfile::tempdir().expect("scratch");
    let store = Store::at(home.path());
    store.save(&state).expect("saved");
    let (loaded, _) = store.load(served).expect("loaded");
    assert_eq!(loaded.account("main").login, state.account("main").login);
    assert!(!signed_out_here(row, &loaded), "the verdict is read back");
    let wall = served + HOUR;
    let again = note_login_end(&mut state, &mut logins, "main", wall);
    assert!(
        again.iter().any(|e| e.kind == Kind::LoginEnded),
        "{again:?}"
    );
    assert!(signed_out_here(row, &state), "a newer wall strikes");
}

/// AN OLD WALL RE-READ ON AN IDLE SCREEN NEVER RESTRIKES: the struck tab's screen still shows the
/// wall hours later, and its point is decided again; each look is the wall's first sighting
/// ([`WallSighting::sighted`]), the reading already kept, so a login another tab has served
/// since stays signed in and nothing is written. CONTROL: once the process served (its sighting
/// cleared), a wall it meets is a new one, dated when it is seen, and strikes.
#[test]
fn an_old_wall_reread_on_an_idle_screen_never_restrikes() {
    let r = roster();
    let row = r.row("main").expect("main");
    let (mut state, mut logins) = renewed(NOW);
    let same = |held: &u32, seen: &u32| held == seen;
    let mut wall = WallSighting::<u32>::default();
    let first = NOW + HOUR;
    let at = wall.sighted(&7, first, same);
    assert_eq!(at, first);
    note_login_end(&mut state, &mut logins, "main", at);
    assert!(signed_out_here(row, &state), "PRECONDITION: struck");
    let served = first + HOUR;
    observe_served(&mut state, &mut logins, "main", served);
    assert!(!signed_out_here(row, &state), "PRECONDITION: served since");
    let before = (state.clone(), logins.clone());
    for later in [served + 60, served + 5 * HOUR, served + 8 * HOUR] {
        let at = wall.sighted(&7, later, same);
        assert_eq!(at, first, "the same wall keeps its first sighting");
        let events = note_login_end(&mut state, &mut logins, "main", at);
        assert!(events.is_empty(), "{events:?}");
        assert!(!signed_out_here(row, &state), "re-read at {later}");
    }
    assert_eq!((&state, &logins), (&before.0, &before.1), "nothing written");
    assert_eq!(
        wall.sighted(&8, served + 9 * HOUR, same),
        served + 9 * HOUR,
        "another process's wall is a new sighting"
    );
    wall.cleared();
    let late = served + 10 * HOUR;
    let at = wall.sighted(&7, late, same);
    assert_eq!(at, late, "CONTROL: a wall after serving is a new sighting");
    note_login_end(&mut state, &mut logins, "main", at);
    assert!(signed_out_here(row, &state), "CONTROL: a new wall strikes");
}

/// The newest reading decides by its own date; in one second a served turn outranks a check, and
/// a strike outranks both ([`Said`]'s order).
#[test]
fn the_newest_reading_decides_and_a_tie_goes_to_the_strike_then_the_served_turn() {
    let mut ledger = LoginLedger::default();
    assert!(ledger.verdict().is_none(), "nothing read");
    assert!(ledger.record(Said::Struck, obs(10, "wall")));
    assert!(ledger.record(Said::Checked(Login::Struck), obs(20, "check")));
    let verdict = ledger.verdict().expect("a verdict");
    assert_eq!(
        (verdict.login, verdict.said),
        (Login::Struck, Said::Checked(Login::Struck))
    );
    assert!(ledger.record(Said::Served, obs(20, "turn")));
    assert_eq!(
        ledger.verdict().map(|v| (v.login, v.said)),
        Some((Login::Serves, Said::Served)),
        "a served turn wins its second over a check"
    );
    assert!(ledger.record(Said::Struck, obs(20, "wall-2")));
    assert_eq!(
        ledger.verdict().map(|v| (v.login, v.said)),
        Some((Login::Struck, Said::Struck)),
        "a strike wins its second"
    );
    assert!(ledger.record(Said::Served, obs(30, "turn-2")));
    assert_eq!(ledger.served_at(), Some(30));
    assert!(
        !ledger.record(Said::Served, obs(25, "turn-3")),
        "an older reading of a kind is not kept"
    );
    assert_eq!(ledger.verdict().map(|v| v.obs.at), Some(30));
}

/// A witness kept already is one reading, whatever date it is read again with.
#[test]
fn a_reading_read_again_is_one_reading_whatever_date_it_carries() {
    let mut ledger = LoginLedger::default();
    assert!(ledger.record(Said::Struck, obs(10, "wall@10")));
    assert!(!ledger.record(Said::Struck, obs(99, "wall@10")));
    assert_eq!(ledger.struck.as_ref().map(|obs| obs.at), Some(10));
    assert!(ledger.record(Said::Checked(Login::Struck), obs(5, "check")));
    assert!(
        !ledger.record(Said::Checked(Login::Struck), obs(5, "check")),
        "the same check"
    );
    assert!(
        !ledger.record(Said::Checked(Login::Serves), obs(4, "older")),
        "an older check is not kept"
    );
}

/// A replaced login's ledger starts empty under the next print, so nothing read of the earlier
/// login is folded with the new one's, and a reading dated before the new login started is of
/// the one it replaced: it is never kept, whatever its kind. CONTROL: a reading of the new
/// login's start or after it is kept.
#[test]
fn a_renewed_ledger_folds_nothing_of_the_earlier_login() {
    let mut ledger = LoginLedger::default();
    ledger.record(Said::Struck, obs(10, "wall"));
    let print = ledger.print();
    ledger.renew(20);
    assert!(ledger.is_empty());
    assert!(ledger.verdict().is_none());
    assert_eq!((ledger.generation, ledger.since), (1, Some(20)));
    assert_ne!(ledger.print(), print);
    for said in [Said::Struck, Said::Served, Said::Checked(Login::Struck)] {
        assert!(!ledger.record(said, obs(19, "earlier")), "{said:?}");
    }
    assert!(ledger.is_empty(), "nothing of the earlier login is kept");
    assert!(ledger.record(Said::Struck, obs(20, "wall-2")), "CONTROL");
    assert_eq!(ledger.verdict().map(|v| v.obs.at), Some(20));
}

/// A word no ledger kept seeds one: the word as a check dated by its time (an undated one at the
/// epoch, which every dated reading outranks), which a check of the same second replaces and a
/// wall or a served turn of that second outranks.
#[test]
fn a_word_no_ledger_kept_seeds_a_record_reading() {
    let struck = LoginLedger::of_word(Auth::SignedOut, Some(50));
    let verdict = struck.verdict().expect("a verdict");
    assert_eq!(
        (
            verdict.login,
            verdict.said,
            verdict.obs.at,
            verdict.obs.source
        ),
        (
            Login::Struck,
            Said::Checked(Login::Struck),
            50,
            Source::Record
        )
    );
    let mut checked = struck.clone();
    assert!(checked.record(Said::Checked(Login::Serves), obs(50, "check")));
    assert_eq!(checked.verdict().map(|v| v.login), Some(Login::Serves));
    let mut ok = LoginLedger::of_word(Auth::Ok, Some(50));
    assert!(ok.record(Said::Struck, obs(50, "wall")));
    assert_eq!(ok.verdict().map(|v| v.login), Some(Login::Struck));
    let mut served = struck.clone();
    assert!(served.record(Said::Served, obs(50, "turn")));
    assert_eq!(served.verdict().map(|v| v.login), Some(Login::Serves));
    let mut undated = LoginLedger::of_word(Auth::SignedOut, None);
    assert!(undated.record(Said::Served, obs(1, "turn")));
    assert_eq!(undated.verdict().map(|v| v.login), Some(Login::Serves));
    assert!(LoginLedger::of_word(Auth::Unseen, Some(7)).is_empty());
}

/// The ledger reads back from its JSON; a leg that does not read as a reading is none.
#[test]
fn a_ledger_reads_back_from_its_json_and_a_bad_leg_is_no_reading() {
    let mut ledger = LoginLedger::default();
    ledger.renew(5);
    ledger.record(Said::Struck, obs(10, "wall"));
    ledger.record(Said::Served, LoginObs::new(20, "turn", Source::Transcript));
    ledger.record(Said::Checked(Login::Struck), obs(15, "check"));
    let cap = Cap::at(trace::Now::from(100));
    assert_eq!(LoginLedger::from_json(&ledger.to_json(), cap), ledger);
    let mut bad = ledger.to_json();
    if let Some(object) = bad.as_object_mut() {
        object.insert(
            "served".into(),
            aterm_json::json!({"at": 20, "witness": "turn", "source": "rumour"}),
        );
    }
    let read = LoginLedger::from_json(&bad, cap);
    assert_eq!((read.served, read.struck), (None, ledger.struck));
}

/// A witness is cut to its bound on a character boundary.
#[test]
fn a_witness_is_cut_on_a_character_boundary() {
    let long = "é".repeat(WITNESS_MAX);
    let cut = LoginObs::new(1, &long, Source::Screen);
    assert!(cut.witness.len() <= WITNESS_MAX);
    assert!(long.starts_with(&cut.witness));
    assert_eq!(cut.witness.len() % 2, 0);
}

/// A store at `root` holding row `work` renewed into its directory `old` at [`NOW`], whose login
/// wall struck it at `wall` and whose login another tab served since, at `served`.
fn struck_then_served(root: &std::path::Path, wall: i64, served: i64) -> Store {
    let store = Store::at_for(root, Cli::Claude, root);
    let old = root.join("old").display().to_string();
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            Ok(note_renewed(state, logins, "work", None, &old, None, NOW))
        })
        .expect("renewed");
    let ended = store
        .transact_logins(wall, &mut None, |state, logins| {
            Ok(note_login_end(state, logins, "work", wall))
        })
        .expect("struck");
    assert!(
        ended.iter().any(|e| e.kind == Kind::LoginEnded),
        "PRECONDITION: the wall ended the login: {ended:?}"
    );
    store
        .transact_logins(served, &mut None, |state, logins| {
            Ok(observe_served(state, logins, "work", served))
        })
        .expect("served");
    store
}

/// Row `work` as the store at `store` binds it now.
fn bound_work(store: &Store, now: i64) -> Row {
    let raw = Roster {
        rows: vec![Row {
            label: "work".into(),
            ..Default::default()
        }],
    };
    let logins = store.load_logins(now).expect("logins").0;
    bound(&raw, &logins).row("work").expect("work").clone()
}

/// AN OLD WALL ASKED AGAIN AFTER ITS LOGIN'S CONTEXT CHANGED STRIKES NOTHING: `work`'s wall
/// struck it, another tab served since, and then its binding moved to another directory with no
/// reading of the login (a context change, which retires the word on record in
/// `auth_scope::finish`). The idle tab whose screen still shows the old wall is asked again on
/// that account news, its wall dated by its first sighting: the new login's evidence started at
/// the change ([`Account::retire_login`], [`LoginLedger::since`]), so a wall older than it is of
/// the login the change replaced, kept nowhere, and it records no word, no end and no `Renew`.
/// CONTROL: a wall first sighted after the change strikes and ends the login.
#[test]
fn an_old_wall_asked_again_after_its_logins_context_changed_strikes_nothing() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let wall = NOW + HOUR;
    let changed = NOW + 3 * HOUR;
    let store = struck_then_served(root.path(), wall, NOW + 2 * HOUR);
    let new = root.path().join("new").display().to_string();
    store
        .transact_logins(changed, &mut None, |_, logins| {
            logins.labels.get_mut("work").expect("bound").dir = Some(new);
            Ok(Vec::new())
        })
        .expect("the context changed");
    let row = bound_work(&store, changed);
    let state = store.load(changed).expect("state").0;
    let account = state.account("work");
    assert_eq!(
        account.current_auth(&row),
        Auth::Unseen,
        "PRECONDITION: the change retired the word"
    );
    assert_eq!(
        (account.login.since, account.login.is_empty()),
        (Some(changed), true),
        "the new login's evidence starts at the change"
    );
    let journal = store.tail(100).expect("journal").len();
    let again = store
        .transact_logins(changed + HOUR, &mut None, |state, logins| {
            Ok(note_login_end(state, logins, "work", wall))
        })
        .expect("asked again");
    assert!(again.is_empty(), "{again:?}");
    let state = store.load(changed + HOUR).expect("state").0;
    assert!(
        !signed_out_here(&row, &state),
        "the old wall struck nothing"
    );
    assert_eq!(store.tail(100).expect("journal").len(), journal);
    let fresh = changed + 2 * HOUR;
    let ended = store
        .transact_logins(fresh, &mut None, |state, logins| {
            Ok(note_login_end(state, logins, "work", fresh))
        })
        .expect("a new wall");
    assert!(
        ended.iter().any(|e| e.kind == Kind::LoginEnded),
        "CONTROL: {ended:?}"
    );
    let state = store.load(fresh).expect("state").0;
    assert!(signed_out_here(&row, &state), "CONTROL: a new wall strikes");
}

/// A SIGN-OUT A CHECK FINDS AFTER SERVED EVIDENCE IS TOLD AT ITS OWN TIME, NEVER AS AN END AT AN
/// OLDER WALL: `main`'s wall struck it at `wall` (its login's end on record), it served an hour
/// later, and four hours on a check found it signed out ([`strike`]'s and a probe's word, which
/// moves no end). The end on record is older than evidence the login served, so no sign-out is
/// told by it ([`sign_out_ended_at`]), though it stays on record as the login's end
/// ([`login_ended_at`]): the journal says `signed-out` since the check, and the account card
/// dates the sign-out by no end. CONTROL: a wall met after that is the login's end, told as
/// `ended` at its own first sighting.
#[test]
fn a_sign_out_a_check_finds_after_served_evidence_is_told_at_its_own_time() {
    let r = roster();
    let row = r.row("main").expect("main");
    let (mut state, mut logins) = renewed(NOW);
    state.accounts.get_mut("main").expect("main").pk = Some("ac-0123456789abcdef".into());
    let wall = NOW + HOUR;
    note_login_end(&mut state, &mut logins, "main", wall);
    observe_served(&mut state, &mut logins, "main", wall + HOUR);
    let checked = wall + 5 * HOUR;
    note_auth(&mut state, "main", Auth::SignedOut, checked);
    assert!(
        signed_out_here(row, &state),
        "PRECONDITION: the check struck"
    );
    assert_eq!(
        (
            logins.get("main").and_then(|b| b.ended_at),
            login_ended_at(row, &logins, &state)
        ),
        (Some(wall), Some(wall)),
        "PRECONDITION: the wall's end stays on record"
    );
    let told = |state: &State, logins: &Logins| {
        let records = AccountRecords {
            raw_roster: r.clone(),
            roster: r.clone(),
            roster_present: true,
            state: state.clone(),
            logins: logins.clone(),
            notes: Vec::new(),
        };
        let rows = super::super::super::journal::login_rows(
            Cli::Claude,
            &records,
            &|_, _| Some("ac-0123456789abcdef".to_owned()),
            &|_| "fill",
            &|_, _| None,
            checked + 2 * HOUR,
        );
        let half = super::super::super::account_card::LoginHalf::of_row(
            row,
            logins,
            state,
            true,
            checked + 2 * HOUR,
        );
        (
            rows.first().map(|row| (row.state, row.since)),
            sign_out_ended_at(row, logins, state),
            half,
        )
    };
    assert_eq!(
        told(&state, &logins),
        (
            Some(("signed-out", u64::try_from(checked).expect("a time"))),
            None,
            super::super::super::account_card::LoginHalf::SignedOut { at: None },
        )
    );
    let again = checked + HOUR;
    note_login_end(&mut state, &mut logins, "main", again);
    assert_eq!(
        told(&state, &logins),
        (
            Some(("ended", u64::try_from(again).expect("a time"))),
            Some(again),
            super::super::super::account_card::LoginHalf::SignedOut { at: Some(again) },
        ),
        "CONTROL: a wall after the check is the login's end"
    );
}
