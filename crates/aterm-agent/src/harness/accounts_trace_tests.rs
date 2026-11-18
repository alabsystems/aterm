// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The store's reads under a trace: what each records, and what moves it.

use super::*;
use aterm_uds::trace::{self, Moved, Now};
use std::time::{Duration, Instant};

/// The store's readers record what they read: a roster's stamp through the bounded reader, a
/// path that held nothing, and a login folder's listing, so a login made in that folder moves
/// the trace.
///
/// NEGATIVE CONTROL (2026-11-15): `read_bounded` without its `trace::fs::opening(path)` failed
/// at the files' record, left `[]`, right `[(".../absent.toml", false),
/// (".../accounts.toml", true)]`; restored, the file touched.
#[test]
fn the_store_readers_record_their_files_a_miss_and_a_listing() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let roster = dir.path().join("accounts.toml");
    let absent = dir.path().join("absent.toml");
    let login = dir.path().join("logins").join("a");
    std::fs::write(&roster, "[[account]]\n").expect("the roster");
    std::fs::create_dir_all(&login).expect("the login folder");
    std::fs::write(login.join(".claude.json"), "{}").expect("its profile");

    let ((text, missing, made), trace) = trace::read_as_if_later(Duration::from_secs(60), || {
        trace::scope(|| {
            (
                roster_text(&roster),
                roster_text(&absent),
                login_made_checked(&login),
            )
        })
    });
    assert_eq!(text, Ok(Some("[[account]]\n".to_owned())));
    assert_eq!(missing, Ok(None));
    assert!(made.expect("the login folder read"));
    assert_eq!(
        trace
            .files()
            .map(|(path, stamp)| (path.to_path_buf(), stamp.is_some()))
            .collect::<Vec<_>>(),
        [(absent.clone(), false), (roster.clone(), true)]
    );
    assert_eq!(trace.listings().collect::<Vec<_>>(), [login.as_path()]);
    assert_eq!(trace.unkept(), None, "{trace:?}");
    let stands = || trace.stands(Now::at(0), Instant::now());
    assert_eq!(stands(), Ok(()));
    std::fs::write(login.join("note"), "").expect("a file beside the login");
    assert_eq!(stands(), Err(Moved::Listing(login.clone())));
}

/// A LOAD THAT MINTS ITS MACHINE ID POISONS THE TRACE, ONCE FOR THE PROCESS. A store that keeps
/// no `machine` file, whose state names no machine, draws one at its first load in the process
/// ([`mint_machine`]): a trace scope armed around that load is poisoned, since no replay draws
/// the same id. Every later load of that store answers the same id and poisons nothing, its trace
/// standing until the store's `machine` file comes; keeping the id ([`Store::machine_id`], given
/// the state's) keeps that one, and loads then read it. Before, each load drew another, so a duty
/// that loaded such a store was poisoned at every pass.
///
/// NEGATIVE CONTROLS:
/// - (2026-11-15) `mint_machine` without its `trace::poison` failed the first load, left `None`,
///   right `Some(Poison("a machine id minted from OS entropy"))`, its trace holding only the
///   store's misses;
/// - (2026-11-16) `Store::state_as_stored` drawing at each load (`mint_machine()` in place of
///   `self.unkept_machine()`) failed `a second load answers the id the first drew`, left
///   `"9b4cfad9f7f92728"`, right `"a29c3a3268765780"`.
///
/// Each restored, the file touched.
#[test]
fn a_load_that_mints_its_machine_id_poisons_the_trace() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let store = Store::at(dir.path());
    store.ensure().expect("the store");
    let load = || {
        trace::read_as_if_later(Duration::from_secs(60), || {
            trace::scope(|| store.load(1_790_000_000))
        })
    };
    let (loaded, minted) = load();
    let (state, _) = loaded.expect("an empty store loads");
    assert!(is_machine_id(&state.machine), "{}", state.machine);
    assert_eq!(
        minted.unkept(),
        Some(trace::Unkept::Poison(
            "a machine id minted from OS entropy".into()
        )),
        "{minted:?}"
    );

    let (loaded, again) = load();
    assert_eq!(
        loaded.expect("loads").0.machine,
        state.machine,
        "a second load answers the id the first drew"
    );
    assert_eq!(again.unkept(), None, "{again:?}");
    assert_eq!(again.stands(Now::at(0), Instant::now()), Ok(()));

    let id = store.machine_id(&state.machine).expect("the id kept");
    assert_eq!(id, state.machine, "the id drawn is the one kept");
    assert_eq!(
        again.stands(Now::at(0), Instant::now()),
        Err(Moved::File(store.machine_file())),
        "the kept id moves the load that read none"
    );
    let (loaded, kept) = load();
    assert_eq!(loaded.expect("loads").0.machine, id);
    assert_eq!(kept.unkept(), None, "CONTROL: {kept:?}");
}

/// A reading in 2027, the second every bound below is placed by.
const AT: i64 = 1_800_000_000;

/// A STATE'S PARSE STANDS ONLY WHILE ITS CLAIMS CLOCK DOES (accounts upkeep on a cause, commit
/// 10b): `State::from_json` keeps the claims clock while its `until` is to come, comparing through
/// the reading it is given ([`Now`]). Read at `until - 1` it keeps the clock, and its trace stands
/// at that second and moves at `until` ([`Moved::Wall`]); read at `until` it keeps none.
///
/// NEGATIVE CONTROL (2026-11-15): the claims clock's filter made unrecorded
/// (`.filter(|clock| trace::unrecorded(|| now.raw()) < clock.until)`) failed `the read that
/// kept the clock stands no longer than the clock`, left `Ok(())`, right `Err(Wall)`; restored,
/// the file touched.
#[test]
fn a_state_read_stands_until_its_claims_clock_ends() {
    let until = AT + 60;
    let state = State {
        clock: Some(Clock {
            standing: ClockStanding::Split,
            by_s: 0,
            against: 2,
            of: 3,
            peers: vec!["m2".to_owned()],
            until,
        }),
        ..State::default()
    };
    let text = state.to_json();
    let (read, trace) = trace::scope(|| State::from_json(&text, until - 1));
    assert_eq!(
        read.expect("the state reads")
            .clock
            .map(|clock| clock.until),
        Some(until),
        "read a second before its end, the clock is kept"
    );
    assert_eq!(trace.unkept(), None, "{trace:?}");
    let mono = Instant::now();
    assert_eq!(trace.stands(Now::at(until - 1), mono), Ok(()));
    assert_eq!(
        trace.stands(Now::at(until), mono),
        Err(Moved::Wall),
        "the read that kept the clock stands no longer than the clock"
    );
    let read = State::from_json(&text, until).expect("the state reads");
    assert!(read.clock.is_none(), "read at its end, the clock is gone");
}

/// A LOGIN STAMP IS READ AS IT IS ONLY WHERE IT IS NO LATER THAN THE READING: `Logins::from_json`
/// reads `renewed_at` as it is at or after that second, and its trace stands from it on; read a
/// second before, it is capped at the reading, a number no comparison states, so the trace is
/// poisoned ([`trace::Unkept::Poison`]).
///
/// NEGATIVE CONTROL (2026-11-15): `Cap::of`'s comparison made unrecorded (`if
/// trace::unrecorded(|| self.now.raw()) >= at.saturating_sub(self.skew)`) failed at the assertion
/// that a read at the stamp moves a second before it, left `Ok(())`, right `Err(Wall)`; restored,
/// the file touched.
#[test]
fn a_login_stamp_past_its_reading_poisons_the_read_and_one_before_stands_from_it_on() {
    let mut state = State::default();
    let mut logins = Logins::default();
    let _ = note_renewed(
        &mut state,
        &mut logins,
        "a",
        Some("/logins/a"),
        "/logins/a",
        Some("max"),
        AT,
    );
    let text = logins.to_json();
    let renewed = |logins: &Logins| logins.get("a").and_then(|binding| binding.renewed_at);

    let (read, trace) = trace::scope(|| Logins::from_json(&text, AT));
    assert_eq!(renewed(&read.expect("the logins read")), Some(AT));
    assert_eq!(trace.unkept(), None, "{trace:?}");
    let mono = Instant::now();
    assert_eq!(trace.stands(Now::at(AT + 3600), mono), Ok(()));
    assert_eq!(
        trace.stands(Now::at(AT - 1), mono),
        Err(Moved::Wall),
        "a read at the stamp moves a second before it"
    );

    let (read, capped) = trace::scope(|| Logins::from_json(&text, AT - 1));
    assert_eq!(
        renewed(&read.expect("the logins read")),
        Some(AT - 1),
        "a stamp past the reading is read as the reading"
    );
    assert_eq!(
        capped.unkept(),
        Some(trace::Unkept::Poison("a raw read of now".into())),
        "{capped:?}"
    );
}

/// THE ROTATION'S READING OF A ROW ON ITS VAULT TOKEN STANDS ONLY WHILE IT IS FRESH: a reading
/// written at `read_at` that no window vouches for names the row through `read_at +
/// TOKENS_HERE_FRESH_S` ([`on_token_where`]), and its trace stands through that second and moves at
/// the next, before the token's own end; there it names the row no more.
///
/// NEGATIVE CONTROL (2026-11-15): the reading's age made unrecorded (`now.aged(read.read_at,
/// TOKENS_HERE_FRESH_S.saturating_add(1))` as `trace::unrecorded(|| now.raw()) - read.read_at >
/// TOKENS_HERE_FRESH_S`) failed `the reading that named the row stands no longer than it is
/// fresh`, left `Ok(())`, right `Err(Wall)`; restored, the file touched.
#[test]
fn a_token_reading_stands_until_it_goes_stale() {
    let row = Row {
        label: "a".to_owned(),
        email: Some("a@example.test".to_owned()),
        ..Row::default()
    };
    let read_at = AT;
    let end = AT + 10 * TOKENS_HERE_FRESH_S;
    let state = State {
        tokens_here: Some(TokensHere {
            read_at,
            ends: BTreeMap::from([("a".to_owned(), end)]),
            vouched: false,
        }),
        ..State::default()
    };
    let last = read_at + TOKENS_HERE_FRESH_S;
    let (on, trace) = trace::scope(|| on_token_where(true, &row, &state, last));
    assert_eq!(on, Some(end), "the last second it is fresh");
    let mono = Instant::now();
    assert_eq!(trace.stands(Now::at(read_at), mono), Ok(()));
    assert_eq!(trace.stands(Now::at(last), mono), Ok(()));
    assert_eq!(
        trace.stands(Now::at(last + 1), mono),
        Err(Moved::Wall),
        "the reading that named the row stands no longer than it is fresh"
    );
    assert_eq!(on_token_where(true, &row, &state, last + 1), None);
}

/// The second each digest line's bound is placed at below, a second past the reading each is
/// first looked at by ([`each_digest_lines_reading_stands_until_the_second_its_line_moves`]).
const LINE_BOUND: i64 = AT;

/// Look at one digest line, `line`, a second before its bound ([`LINE_BOUND`]) under a trace, and
/// again at its bound: the first trace is kept, stands through that second and no later, and the
/// line answers otherwise at its bound.
fn line_moves_at_its_bound<R: PartialEq + std::fmt::Debug>(name: &str, line: impl Fn(Now) -> R) {
    let (before, trace) = trace::scope(|| line(Now::at(LINE_BOUND - 1)));
    assert_eq!(trace.unkept(), None, "{name}: {trace:?}");
    let mono = Instant::now();
    assert_eq!(
        trace.stands(Now::at(LINE_BOUND - 1), mono),
        Ok(()),
        "{name}: the reading stands through its own second"
    );
    assert_eq!(
        trace.stands(Now::at(LINE_BOUND), mono),
        Err(Moved::Wall),
        "{name}: the reading stands no longer than its line, {before:?}"
    );
    assert_ne!(
        line(Now::at(LINE_BOUND)),
        before,
        "{name}: the line moves at its bound"
    );
}

/// EACH DIGEST LINE'S READING STANDS UNTIL THE SECOND ITS LINE MOVES (accounts upkeep on a cause,
/// commit 10d). The window's look at the accounts (aterm-gui's `AccountsSaid::listed`) compares
/// every time its lines read through the look's reading ([`Now`]): a credits episode's end
/// ([`credits_noted`]), the week's reset coming within a day with credits off ([`windows_full`]),
/// the clock record's lapse ([`State::clock_at`]), the last working login's end coming within a
/// day ([`none_working_within`]), and a low window's reset, which moves the row's
/// [`condition`] (the signed-out line's and the no-sign-in line's). Each line, read a second
/// before its bound, keeps a trace that stands through that second and moves at the bound, where
/// the line answers otherwise.
///
/// NEGATIVE CONTROL (2026-11-16): `CreditsUse::stands` made unrecorded
/// (`trace::unrecorded(|| now.into().raw()) < self.until_at()`) failed `credits: the reading stands
/// no longer than its line, ["a"]`, left `Ok(())`, right `Err(Wall)`, the credits episode's
/// reading standing at its end; restored, the file touched.
#[test]
fn each_digest_lines_reading_stands_until_the_second_its_line_moves() {
    let row = |label: &str| Row {
        label: label.to_owned(),
        email: Some(format!("{label}@example.test")),
        ..Row::default()
    };
    let one = |label: &str| Roster {
        rows: vec![row(label)],
    };
    let with = |label: &str, account: Account| State {
        accounts: BTreeMap::from([(label.to_owned(), account)]),
        ..State::default()
    };

    let roster = one("a");
    let state = with(
        "a",
        Account {
            credits: Some(Credits::InUse(CreditsUse {
                since: LINE_BOUND - CREDITS_STALE_S,
                seen: LINE_BOUND - CREDITS_STALE_S,
                until: Some(LINE_BOUND),
                told: true,
            })),
            ..Account::default()
        },
    );
    line_moves_at_its_bound("credits", |now| credits_noted(&roster, &state, now));

    let roster = one("b");
    let state = with(
        "b",
        Account {
            credits: Some(Credits::Off(CreditsOff {
                reason: "out_of_credits".to_owned(),
                at: LINE_BOUND - DAY_S,
                until: None,
            })),
            warned_week: Some(WindowSaid {
                pct: Some(WINDOW_FULL_PCT),
                reset: Some(LINE_BOUND + WINDOW_FULL_AHEAD_S),
                at: LINE_BOUND - DAY_S,
            }),
            ..Account::default()
        },
    );
    line_moves_at_its_bound("window full", |now| windows_full(&roster, &state, now));

    let state = State {
        clock: Some(Clock {
            standing: ClockStanding::Split,
            by_s: 0,
            against: 2,
            of: 3,
            peers: vec!["m99".to_owned()],
            until: LINE_BOUND,
        }),
        ..State::default()
    };
    line_moves_at_its_bound("clock", |now| state.clock_at(now).cloned());

    let roster = Roster {
        rows: vec![Row {
            dir: Some("/nonexistent/logins/c".to_owned()),
            ..row("c")
        }],
    };
    let (mut state, mut logins) = (State::default(), Logins::default());
    let _ = note_renewed(
        &mut state,
        &mut logins,
        "c",
        Some("/nonexistent/logins/c"),
        "/nonexistent/logins/c",
        None,
        LINE_BOUND + NONE_SOON_S - LIFETIME_START_S,
    );
    let enrolled = BTreeSet::from(["c".to_owned()]);
    line_moves_at_its_bound("none soon", |now| {
        none_working_within(&roster, &logins, &state, &enrolled, now, NONE_SOON_S)
    });

    let low = Row {
        email: None,
        ..row("d")
    };
    let state = with(
        "d",
        Account {
            sample: Some(Sample {
                session_pct: Some(LOW_SESSION_PCT),
                session_reset: Some(LINE_BOUND),
                at: LINE_BOUND - 3600,
                ..Sample::default()
            }),
            ..Account::default()
        },
    );
    line_moves_at_its_bound("a low window", |now| condition(&low, &state, true, now));
}
