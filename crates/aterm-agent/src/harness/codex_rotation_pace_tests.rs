// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One asking process's reads at a usage limit go by rungs ([`ReadPace`]), and none is made while
//! a selection could not be published. A row "never taken" answers usable on the session's own
//! account, which the handoff refuses after reading it. Times are seconds after `NOW`.

use super::*;
use crate::harness::codex_rotation_pace::{
    ASKING_S, Climb, Minted, OwnRead, PACE_S, ReadPace, Run, Wait,
};
use std::cell::Cell;
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};

type Asked = Result<Option<codex_rotation::Proposal>, &'static str>;

/// One ask at `at` of the process whose reads so far are `pace`.
fn ask(world: &World, reader: &dyn UsageReader, pace: &mut ReadPace, at: i64) -> Asked {
    let seat = Seat {
        now: at,
        ..world.seat()
    };
    let mut session = asked_session(world);
    session.point = codex_rotation::wall_asked(at);
    codex_rotation::prepare_paced(
        &seat,
        &session,
        &Identity::from_native("source", "user"),
        reader,
        Duration::from_secs(60),
        pace,
    )
}

/// Asks at each of `offsets`; the offsets of the asks that read, each with the logins it read.
fn passes(
    world: &World,
    reader: &Reader,
    pace: &mut ReadPace,
    offsets: impl IntoIterator<Item = i64>,
) -> Vec<(i64, Vec<PathBuf>)> {
    passes_of(world, reader, reader, pace, offsets)
}

/// [`passes`] through `asked`, a reader that counts its reads in `reader`.
fn passes_of(
    world: &World,
    asked: &dyn UsageReader,
    reader: &Reader,
    pace: &mut ReadPace,
    offsets: impl IntoIterator<Item = i64>,
) -> Vec<(i64, Vec<PathBuf>)> {
    let mut read = Vec::new();
    for offset in offsets {
        let before = reader.calls.borrow().len();
        let _ = ask(world, asked, pace, NOW + offset);
        let made = reader.calls.borrow()[before..].to_vec();
        if !made.is_empty() {
            read.push((offset, made));
        }
    }
    read
}

/// An ask every five seconds, as a client at its wall makes them: from `from` up to, not at, `to`.
fn every_5s(from: i64, to: i64) -> impl Iterator<Item = i64> {
    (from..to).step_by(5)
}

/// How many of `reader`'s reads were of the login in `file`.
fn reads_of(reader: &Reader, file: &Path) -> usize {
    reader
        .calls
        .borrow()
        .iter()
        .filter(|read| read.as_path() == file)
        .count()
}

/// The two-row world of [`fixture`] whose `work` holds a login a context can be captured of, and
/// answers usable on the session's own account: never taken.
fn never_taken(name: &str) -> (World, Reader, PathBuf, PathBuf) {
    let (world, mut reader) = fixture(name);
    let (main, work) = (
        main_of(&world),
        world.work_dir(Cli::Codex).join("auth.json"),
    );
    native_auth(&work, "target");
    reader.answers.insert(work.clone(), usage("source", false));
    (world, reader, main, work)
}

/// A world whose roster is `main`, the session's row on the default login, and `labels` in that
/// order, each with a login file of its own; those files. Nothing answers yet but `main`, at its
/// weekly limit.
fn pool(name: &str, labels: &[&str]) -> (World, Reader, Vec<PathBuf>) {
    let world = World::new(name);
    let files = enroll(&world, labels);
    world.on_main(Cli::Codex, &[]);
    let reader = Reader {
        calls: RefCell::new(Vec::new()),
        answers: BTreeMap::from([(main_of(&world), usage("source", true))]),
    };
    (world, reader, files)
}

/// Writes `world`'s roster as `main` and `labels`, and each label's login file; those files.
fn enroll(world: &World, labels: &[&str]) -> Vec<PathBuf> {
    let mut roster = String::from("[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n");
    let mut files = Vec::new();
    for label in labels {
        let dir = world.root.join("logins/codex").join(label);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("auth.json");
        if !file.exists() {
            native_auth(&file, label);
        }
        write!(
            roster,
            "\n[[account]]\nlabel = \"{label}\"\nclass = \"dedicated\"\ndir = {}\n",
            toml(&dir)
        )
        .unwrap();
        files.push(file);
    }
    std::fs::write(world.cfg.join(Cli::Codex.accounts().roster_file), roster).unwrap();
    files
}

/// Writes the login file at `path` again with another refresh token, as a renewal of it does.
fn renew(path: &Path, token: &str) {
    native_auth(path, "target");
    let login = std::fs::read_to_string(path).unwrap();
    let renewed = login.replace(
        r#""refresh_token":"fixture""#,
        &format!(r#""refresh_token":"{token}""#),
    );
    assert_ne!(login, renewed, "the fixture's token was replaced");
    std::fs::write(path, renewed).unwrap();
}

/// [`Reader`], with `during` run inside each read once the read is counted.
struct Hooked<'a> {
    reader: &'a Reader,
    during: &'a dyn Fn(&Seat<'_>, &Path),
}

impl UsageReader for Hooked<'_> {
    fn read_on(
        &self,
        seat: &Seat<'_>,
        home: &Path,
        expected_email: Option<&str>,
        now: i64,
        wait: Duration,
        use_token: crate::harness::provider_read::Use,
    ) -> IdentifiedRead {
        let read = self.reader.read(home, expected_email, now, wait, use_token);
        (self.during)(seat, home);
        read
    }

    fn read(
        &self,
        home: &Path,
        expected_email: Option<&str>,
        now: i64,
        wait: Duration,
        use_token: crate::harness::provider_read::Use,
    ) -> IdentifiedRead {
        self.reader.read(home, expected_email, now, wait, use_token)
    }
}

/// A USABLE ROW NEVER TAKEN IS READ BY RUNGS, NOT AT EACH ASK: its client asks every five seconds
/// for as long as its wall stands. With `work` never taken, an hour of asks reads the session's
/// login and `work` four times, at 0, 60, 360 and 2160 s, and a day of them fifty times. CONTROL:
/// the same asks, each of a process that has read nothing, read at every one.
#[test]
fn a_usable_row_never_taken_is_read_by_rungs() {
    let (world, reader, main, work) = never_taken("pace-never-taken");
    let mut pace = ReadPace::new();
    let both = vec![main, work];
    let read = passes(&world, &reader, &mut pace, every_5s(0, HOUR));
    assert_eq!(
        read,
        [0, 60, 360, 2160].map(|at| (at, both.clone())),
        "{pace:?}"
    );
    let _ = passes(&world, &reader, &mut pace, every_5s(HOUR, 24 * HOUR));
    assert_eq!(reader.calls.borrow().len(), 100, "a day: {pace:?}");

    let (world, reader, ..) = never_taken("pace-never-taken-unpaced");
    for at in every_5s(0, 100) {
        let _ = ask(&world, &reader, &mut ReadPace::new(), NOW + at);
    }
    assert_eq!(reader.calls.borrow().len(), 40, "CONTROL");
}

/// SEVERAL ROWS NEVER TAKEN COST THE SESSION'S OWN LOGIN ITS LADDER, NOT ONE EACH: with five such
/// rows, of which a pass reads three, an hour of asks reads the session's login six times: twice
/// with rows it had not read, and four times on its own ladder, however the rows' own rungs fall.
/// Each of the first three rows is read four times and the last two twice.
#[test]
fn several_never_taken_rows_cost_the_own_login_its_ladder_not_theirs() {
    let labels = ["t1", "t2", "t3", "t4", "t5"];
    let (world, mut reader, files) = pool("pace-five-never-taken", &labels);
    for file in &files {
        reader.answers.insert(file.clone(), usage("source", false));
    }
    let main = main_of(&world);
    let mut pace = ReadPace::new();
    let read = passes(&world, &reader, &mut pace, every_5s(0, HOUR));
    let with = |rows: &[usize]| {
        let mut logins = vec![main.clone()];
        logins.extend(rows.iter().map(|row| files[*row].clone()));
        logins
    };
    assert_eq!(
        read,
        [
            (0, with(&[0, 1, 2])),
            (5, with(&[3, 4])),
            (60, with(&[0, 1, 2])),
            (120, with(&[3, 4])),
            (420, with(&[0, 1, 2])),
            (2220, with(&[0, 1, 2])),
        ],
        "{pace:?}"
    );
    let first_reads = 2;
    assert!(reads_of(&reader, &main) <= 4 + first_reads, "{pace:?}");
    assert_eq!(reader.calls.borrow().len(), 22);
}

/// NEWS BEHIND ROWS ALREADY READ IS READ WHILE THE OWN LOGIN'S LADDER STANDS: in the world of five
/// rows never taken, at 70 s the last two rows' rungs have ended and the own login's ladder
/// stands to 120 s. A row signed in then is read with the session's login at the next ask: the
/// ladder hides the rows already read usable, so the forecast names the new one past them.
#[test]
fn news_behind_rows_already_read_is_read_while_the_own_ladder_stands() {
    let labels = ["t1", "t2", "t3", "t4", "t5"];
    let (world, mut reader, files) = pool("pace-news-behind", &labels);
    for file in &files {
        reader.answers.insert(file.clone(), usage("source", false));
    }
    let main = main_of(&world);
    let mut pace = ReadPace::new();
    let read = passes(&world, &reader, &mut pace, every_5s(0, 70));
    assert_eq!(
        read.iter().map(|(at, _)| *at).collect::<Vec<_>>(),
        [0, 5, 60]
    );
    assert!(pace.holds(&main, true, NOW + 70), "CONTROL: {pace:?}");
    assert!(
        pace.rows["t4"].run.stands_until() <= NOW + 70,
        "CONTROL: {pace:?}"
    );
    let new = enroll(&world, &["t1", "t2", "t3", "t4", "t5", "t6"])
        .pop()
        .unwrap();
    reader.answers.insert(new.clone(), usage("source", false));
    let read = passes(&world, &reader, &mut pace, [70]);
    assert_eq!(read, [(70, vec![main, new])], "{pace:?}");
}

/// A SWEEP OF UNREAD ROWS IS NOT PACED: seven candidates, of which six read walled and the last
/// can take the session. Three asks five seconds apart read three, three and one of them, and the
/// third proposes the move. No pass was a repeat, so the own login's ladder never began, and a
/// row read walled is not one a later read would repeat.
#[test]
fn a_sweep_of_unread_rows_is_not_paced() {
    let labels = ["t1", "t2", "t3", "t4", "t5", "t6", "t7"];
    let (world, mut reader, files) = pool("pace-sweep", &labels);
    for (label, file) in labels.iter().zip(&files).take(6) {
        reader.answers.insert(file.clone(), usage(label, true));
    }
    reader
        .answers
        .insert(files[6].clone(), usage("target", false));
    let mut pace = ReadPace::new();
    let mut made = Vec::new();
    let mut proposed = None;
    for at in [0, 5, 10] {
        let before = reader.calls.borrow().len();
        proposed = ask(&world, &reader, &mut pace, NOW + at).unwrap();
        made.push(reader.calls.borrow().len() - before);
    }
    assert_eq!(made, [4, 4, 2], "{:?}", reader.calls);
    assert_eq!(
        proposed.map(|proposal| proposal.destination.label),
        Some("t7".into())
    );
    assert!(pace.own.is_none(), "{pace:?}");
    assert!(!pace.asked("t1"), "{pace:?}");
}

/// NEWS IS READ PAST A STANDING LADDER. With `work` never taken until the own login's ladder is on
/// its 30 minute rung (to 3960 s), each of these is read at the first ask after it: `spare`,
/// walled on record and never read by this process, once its wall ends; `spare`, read walled by
/// this process, once that wall ends; and `work` itself once its login file holds another token.
/// CONTROL: no ask before it reads.
#[test]
fn news_is_read_past_a_standing_ladder() {
    for read_walled in [false, true] {
        let (world, mut reader, files) = pool("pace-news-wall-ends", &["work", "spare"]);
        let (main, work, spare) = (main_of(&world), files[0].clone(), files[1].clone());
        reader.answers.insert(work.clone(), usage("source", false));
        if read_walled {
            reader.answers.insert(
                spare.clone(),
                usage_resetting("spare", (100, 0), Some(NOW + 2400), NOW),
            );
        } else {
            wall(&world, Cli::Codex, "spare", NOW + 2400);
        }
        let mut pace = ReadPace::new();
        let read = passes(&world, &reader, &mut pace, every_5s(0, 2400));
        assert_eq!(
            read.iter().map(|(at, _)| *at).collect::<Vec<_>>(),
            [0, 60, 360, 2160],
            "{read_walled}: {pace:?}"
        );
        assert_eq!(
            reads_of(&reader, &spare),
            usize::from(read_walled),
            "{read_walled}"
        );
        assert!(pace.holds(&main, true, NOW + 3955), "CONTROL: {pace:?}");
        reader.answers.insert(spare.clone(), usage("target", false));
        let ends = 2400 + accounts::RESET_GRACE_S;
        let read = passes(&world, &reader, &mut pace, every_5s(2400, ends));
        assert!(read.is_empty(), "{read_walled} CONTROL: the wall stands");
        let proposal = ask(&world, &reader, &mut pace, NOW + ends).unwrap();
        assert_eq!(
            proposal.map(|proposal| proposal.destination.label),
            Some("spare".into()),
            "{read_walled}: {pace:?}"
        );
        let calls = reader.calls.borrow();
        assert_eq!(calls[calls.len() - 2..], [main, spare], "{read_walled}");
    }

    let (world, reader, main, work) = never_taken("pace-news-login-replaced");
    let mut pace = ReadPace::new();
    let read = passes(&world, &reader, &mut pace, every_5s(0, 2500));
    assert_eq!(read.len(), 4, "{pace:?}");
    renew(&work, "another");
    let read = passes(&world, &reader, &mut pace, every_5s(2500, 2520));
    assert_eq!(read, [(2500, vec![main, work])], "{pace:?}");
}

/// A PASS ITS OWN LOGIN'S READ ENDS PACES THAT LOGIN, HOWEVER MANY ROWS ARE UNREAD: where the
/// session's login answers as another identity, and where the session holds a login its row no
/// longer does and that read answers nothing (so nothing is on the row's record to stop the next
/// ask), an hour of asks reads that login four times, at 0, 60, 360 and 2160 s, and no candidate.
/// The unread rows are news at every ask, and no news changes what that login answers.
#[test]
fn a_pass_its_own_read_ends_paces_the_own_login() {
    let labels = ["t1", "t2", "t3", "t4", "t5"];
    let (world, mut reader, files) = pool("pace-own-other-identity", &labels);
    for file in &files {
        reader.answers.insert(file.clone(), usage("target", false));
    }
    let main = main_of(&world);
    reader.answers.insert(main.clone(), usage("another", true));
    let mut pace = ReadPace::new();
    assert_eq!(
        ask(&world, &reader, &mut pace, NOW).err(),
        Some("source-identity-changed")
    );
    let read = passes(&world, &reader, &mut pace, every_5s(5, HOUR));
    assert_eq!(
        read,
        [60, 360, 2160].map(|at| (at, vec![main.clone()])),
        "{pace:?}"
    );

    let (world, mut reader) = fixture("pace-own-replaced-login");
    reader.answers.remove(&main_of(&world));
    let moved = world.root.join("logins/codex/main-renewed");
    std::fs::create_dir_all(&moved).unwrap();
    std::fs::write(moved.join("auth.json"), "{}").unwrap();
    world
        .seat()
        .store(Cli::Codex)
        .transact_logins(NOW, &mut None, |_, logins| {
            logins.labels.insert(
                "main".to_string(),
                accounts::Binding {
                    dir: Some(moved.display().to_string()),
                    was: vec![None],
                    ..accounts::Binding::default()
                },
            );
            Ok(Vec::new())
        })
        .unwrap();
    let mut pace = ReadPace::new();
    let read = passes(&world, &reader, &mut pace, every_5s(0, HOUR));
    assert_eq!(
        read,
        [0, 60, 360, 2160].map(|at| (at, vec![main_of(&world)])),
        "{pace:?}"
    );
    let record = world.load(Cli::Codex).accounts.get("main").cloned();
    assert_eq!(record.and_then(|account| account.unanswered), None);
}

/// A ROW NAMED AND NEVER REACHED COUNTS AS ASKED: a session whose program keeps no row's login
/// its own has no line to any row, so each pass reads its login at its limit and no candidate,
/// while the forecast goes on naming `work`. That pass is a repeat, and an hour of asks makes it
/// four times, at 0, 60, 360 and 2160 s. CONTROL: under a program that keeps a row's login, the
/// first ask proposes `work`.
#[test]
fn a_row_named_and_never_reached_counts_as_asked() {
    let (world, reader) = fixture("pace-no-line");
    let keeps_none = crate::harness::codex_alab::lay_package(&world.root.join("store/none"), false);
    let asked = |exe: &Path, pace: &mut ReadPace, at: i64| {
        let seat = Seat {
            now: NOW + at,
            ..world.seat()
        };
        let mut session = session(Cli::Codex, Supervisor::Own, Mode::Early, exe);
        session.point = codex_rotation::wall_asked(NOW + at);
        codex_rotation::prepare_paced(
            &seat,
            &session,
            &Identity::from_native("source", "user"),
            &reader,
            Duration::from_secs(60),
            pace,
        )
    };
    let mut pace = ReadPace::new();
    let mut read = Vec::new();
    for at in every_5s(0, HOUR) {
        let before = reader.calls.borrow().len();
        assert!(
            matches!(asked(&keeps_none, &mut pace, at), Ok(None)),
            "{at}"
        );
        if reader.calls.borrow().len() > before {
            read.push(at);
        }
    }
    assert_eq!(read, [0, 60, 360, 2160], "{pace:?}");
    assert_eq!(reads_of(&reader, &main_of(&world)), 4, "{:?}", reader.calls);
    assert!(pace.asked("work"), "{pace:?}");

    let proposal = asked(&world.exe, &mut ReadPace::new(), HOUR).unwrap();
    assert_eq!(
        proposal.map(|proposal| proposal.destination.label),
        Some("work".into()),
        "CONTROL"
    );
}

/// A HELD SIGN-IN TURN OR A FORBIDDING `aterm.toml` READS NOTHING AND SPENDS NO RUNG: a selection
/// could not be published under either, so no login's token is presented for one. A hundred asks
/// under each read nothing, and the first ask after it ends reads both logins and proposes the
/// move, which a rung spent on the refused asks would hold. THE TURN IS NOT HELD ACROSS A READ:
/// it is free while the session's login and the candidate are read.
#[test]
fn a_held_sign_in_turn_or_a_forbidding_policy_reads_nothing_and_spends_no_rung() {
    let moves = |asked: Asked| asked.unwrap().map(|proposal| proposal.destination.label);
    let (world, reader) = fixture("pace-sign-in-turn-held");
    let mut pace = ReadPace::new();
    let turn = world
        .seat()
        .store(Cli::Codex)
        .signin_within()
        .unwrap()
        .unwrap();
    let read = passes(&world, &reader, &mut pace, every_5s(0, 500));
    assert!(read.is_empty(), "the turn is held: {read:?}");
    drop(turn);
    assert_eq!(
        moves(ask(&world, &reader, &mut pace, NOW + 500)),
        Some("work".into())
    );
    assert_eq!(reader.calls.borrow().len(), 2);

    let (world, reader) = fixture("pace-policy-forbids");
    let config = world.cfg.join("aterm.toml");
    aterm_uds::atomic_file::write(&config, b"[harness]\nrelaunch = false\n", 0o600).unwrap();
    let mut pace = ReadPace::new();
    let read = passes(&world, &reader, &mut pace, every_5s(0, 500));
    assert!(read.is_empty(), "aterm.toml forbids a handoff: {read:?}");
    aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    assert_eq!(
        moves(ask(&world, &reader, &mut pace, NOW + 500)),
        Some("work".into())
    );
    assert_eq!(reader.calls.borrow().len(), 2);

    let (world, reader) = fixture("pace-sign-in-turn-free");
    let free = Cell::new(0);
    let during = |seat: &Seat<'_>, _: &Path| {
        let turn = seat.store(Cli::Codex).signin_now();
        assert!(matches!(turn, Ok(Some(_))), "held across a read");
        free.set(free.get() + 1);
    };
    let hooked = Hooked {
        reader: &reader,
        during: &during,
    };
    assert_eq!(
        moves(ask(&world, &hooked, &mut ReadPace::new(), NOW)),
        Some("work".into())
    );
    assert_eq!(free.get(), 2, "both reads were watched");
}

/// A ROW THAT ANSWERS NO FIGURE AND A USABLE ONE BY TURNS STILL CLIMBS: every read of a row counts
/// toward its rung, whatever it answered, though a figure ends the run on its record. An hour of
/// asks reads `work` four times.
#[test]
fn an_alternating_unanswered_and_usable_row_still_climbs() {
    let (world, mut reader, _, work) = never_taken("pace-alternating");
    let mut pace = ReadPace::new();
    let mut read = Vec::new();
    for at in every_5s(0, HOUR) {
        let made = reads_of(&reader, &work);
        if made.is_multiple_of(2) {
            reader.answers.remove(&work);
        } else {
            reader.answers.insert(work.clone(), usage("source", false));
        }
        let _ = ask(&world, &reader, &mut pace, NOW + at);
        if reads_of(&reader, &work) > made {
            read.push(at);
        }
    }
    assert_eq!(read, [0, 60, 360, 2160], "{pace:?}");
}

/// EACH PROCESS HAS ITS OWN PACE AND ALL SHARE THE RECORDS: two processes asking side by side,
/// `work` never taken, each make the four passes of an hour. And where `work` reads walled, the
/// record of the first process's read stops the second's first ask before any read.
#[test]
fn each_process_has_its_own_pace_and_shares_the_records() {
    let (world, reader, ..) = never_taken("pace-two-processes");
    let (mut first, mut second) = (ReadPace::new(), ReadPace::new());
    for at in every_5s(0, HOUR) {
        let _ = ask(&world, &reader, &mut first, NOW + at);
        let _ = ask(&world, &reader, &mut second, NOW + at);
    }
    assert_eq!(reader.calls.borrow().len(), 16, "{first:?} {second:?}");

    let (world, mut reader, _, work) = never_taken("pace-two-processes-one-record");
    reader.answers.insert(work, usage("target", true));
    assert!(matches!(
        ask(&world, &reader, &mut ReadPace::new(), NOW),
        Ok(None)
    ));
    assert_eq!(reader.calls.borrow().len(), 2, "CONTROL");
    assert!(matches!(
        ask(&world, &reader, &mut ReadPace::new(), NOW + 5),
        Ok(None)
    ));
    assert_eq!(reader.calls.borrow().len(), 2, "the wall is on record");
}

/// A SELECTED ROW IS PACED LIKE ANY READ: a selection that is never committed is made again only
/// by rungs. `work` can take the session: the first ask proposes it, the asks of the next minute
/// read nothing, the ask at 60 s reads and proposes it again, and the next read is at 360 s. Each
/// of those reads is on the row's own run.
#[test]
fn a_selected_row_is_paced_like_any_read() {
    let (world, reader) = fixture("pace-selected");
    let mut pace = ReadPace::new();
    let mut proposed = Vec::new();
    for at in every_5s(0, 400) {
        if let Ok(Some(proposal)) = ask(&world, &reader, &mut pace, NOW + at) {
            assert_eq!(proposal.destination.label, "work");
            proposed.push((at, reader.calls.borrow().len()));
        }
    }
    assert_eq!(proposed, [(0, 2), (60, 4), (360, 6)], "{pace:?}");
    assert_eq!(pace.rows["work"].run.attempts, 3, "{pace:?}");
}

/// A READING OF NO WALL RESTARTS THE OWN LOGIN'S LADDER AT ITS FIRST RUNG: with that ladder on its
/// 30 minute rung, an ask whose read shows the session's login with room answers `not-walled`
/// with that one read. The login full again, the asks of the next minute read nothing, and the
/// ask a minute after that reading reads it and `work`: the ladder neither stood where it was nor
/// climbed.
#[test]
fn a_reading_of_no_wall_restarts_the_own_ladder_at_its_first_rung() {
    let (world, mut reader, main, work) = never_taken("pace-no-wall");
    let mut pace = ReadPace::new();
    let read = passes(&world, &reader, &mut pace, every_5s(0, 3960));
    assert_eq!(read.len(), 4, "{pace:?}");
    reader.answers.insert(main.clone(), usage("source", false));
    assert_eq!(
        ask(&world, &reader, &mut pace, NOW + 3960).err(),
        Some("not-walled")
    );
    assert_eq!(&reader.calls.borrow()[8..], std::slice::from_ref(&main));
    reader.answers.insert(main.clone(), usage("source", true));
    let read = passes(&world, &reader, &mut pace, every_5s(3965, 4025));
    assert_eq!(read, [(4020, vec![main, work])], "{pace:?}");
}

/// A LOGIN THAT KEEPS READING NO WALL IS READ ONCE A MINUTE AT MOST: a pass that shows no limit to
/// leave climbs nothing, and by this pace alone it is made again after a minute, however many
/// rows are unread. Asked every five seconds for an hour, with every read of the session's login
/// showing room, the pace lets sixty reads through, none of a candidate.
#[test]
fn a_login_that_keeps_reading_no_wall_is_read_once_a_minute() {
    let (world, mut reader, files) = pool("pace-never-walled", &["t1", "t2", "t3"]);
    for file in &files {
        reader.answers.insert(file.clone(), usage("target", false));
    }
    let main = main_of(&world);
    reader.answers.insert(main.clone(), usage("source", false));
    let mut pace = ReadPace::new();
    let read = passes(&world, &reader, &mut pace, every_5s(0, HOUR));
    assert_eq!(
        read,
        (0..60)
            .map(|minute| (60 * minute, vec![main.clone()]))
            .collect::<Vec<_>>(),
        "{pace:?}"
    );
}

/// THE PACE KEEPS THE LOGIN ITS READ LEFT, AND A FILE THAT GIVES NONE IS NO CHANGE: a read that
/// renews `work`'s token leaves another token in its file, and the next ask still leaves the row
/// unread; so does one that finds the file unreadable as a login.
#[test]
fn the_pace_keeps_the_login_it_read() {
    let (world, reader, _, work) = never_taken("pace-renewing-read");
    let store = world.seat().store(Cli::Codex);
    let row = world.seat().bound_roster(Cli::Codex).unwrap().rows[1].clone();
    let before = store.capture_auth_context(&row);
    let during = |_: &Seat<'_>, home: &Path| {
        if home == work.as_path() {
            renew(&work, "renewed");
        }
    };
    let hooked = Hooked {
        reader: &reader,
        during: &during,
    };
    let mut pace = ReadPace::new();
    let read = passes_of(&world, &hooked, &reader, &mut pace, [0, 5]);
    assert_eq!(read.iter().map(|(at, _)| *at).collect::<Vec<_>>(), [0]);
    let after = store.capture_auth_context(&row);
    assert!(before.is_some() && after.is_some() && before != after);

    let (world, reader, _, work) = never_taken("pace-unreadable-login");
    let mut pace = ReadPace::new();
    assert!(matches!(ask(&world, &reader, &mut pace, NOW), Ok(None)));
    std::fs::write(&work, "{}").unwrap();
    let read = passes(&world, &reader, &mut pace, [5]);
    assert!(read.is_empty(), "{pace:?}");
}

/// A READING THAT GOES UNRECORDED IS READ BY RUNGS: a row that answers walled is news at every
/// ask, and one that answers no figure is held by the run on its record, so the record is what
/// stops them. Where that reading is not recorded (its login changed during the read, so the
/// record refuses it), the row's own rung is the only stop, whichever it answered: an hour of
/// asks reads it four times, at 0, 60, 360 and 2160 s. A walled row stays news; one that
/// answered no figure is a repeat.
#[test]
fn a_reading_that_goes_unrecorded_is_read_by_rungs() {
    for walled in [true, false] {
        unrecorded_reading_is_read_by_rungs(walled);
    }
}

/// [`a_reading_that_goes_unrecorded_is_read_by_rungs`] for a row that answers `walled`, else no
/// figure.
fn unrecorded_reading_is_read_by_rungs(walled: bool) {
    let (world, mut reader, main, work) = never_taken("pace-unrecorded-reading");
    if walled {
        reader.answers.insert(work.clone(), usage("target", true));
    } else {
        reader.answers.remove(&work);
    }
    let renewals = Cell::new(0);
    let during = |_: &Seat<'_>, home: &Path| {
        if home == work.as_path() {
            renewals.set(renewals.get() + 1);
            renew(&work, &format!("renewed-{}", renewals.get()));
        }
    };
    let hooked = Hooked {
        reader: &reader,
        during: &during,
    };
    let mut pace = ReadPace::new();
    let read = passes_of(&world, &hooked, &reader, &mut pace, every_5s(0, HOUR));
    assert_eq!(
        read,
        [0, 60, 360, 2160].map(|at| (at, vec![main.clone(), work.clone()])),
        "{walled}: {pace:?}"
    );
    assert!(
        world
            .load(Cli::Codex)
            .accounts
            .get("work")
            .is_none_or(|account| account.walls.is_empty() && account.unanswered.is_none()),
        "{walled} CONTROL: nothing of `work`'s reads is on record"
    );
    assert_eq!(pace.asked("work"), !walled, "{pace:?}");
}

/// A ROW READ WALLED ON RECORD IS READ WHEN ITS WALL ENDS, WHATEVER ITS RUN HAD CLIMBED TO: `work`
/// is read usable and not taken at 0, 60 and 360 s, and that third read answers a five-hour wall
/// resetting at 960 s. The wall goes on record, which gates the row from there; the first ask
/// after the wall and its grace reads `work` again, not the one at 2160 s its third rung ends at.
#[test]
fn a_row_read_walled_on_record_is_read_when_its_wall_ends() {
    let (world, mut reader, main, work) = never_taken("pace-wall-ends-on-record");
    let mut pace = ReadPace::new();
    let both = vec![main, work.clone()];
    let read = passes(&world, &reader, &mut pace, every_5s(0, 360));
    assert_eq!(read, [0, 60].map(|at| (at, both.clone())));
    reader.answers.insert(
        work.clone(),
        usage_resetting("source", (100, 0), Some(NOW + 960), NOW + 360),
    );
    let read = passes(&world, &reader, &mut pace, [360]);
    assert_eq!(read, [(360, both.clone())]);
    assert!(
        world.load(Cli::Codex).accounts["work"]
            .limited_until(NOW + 360)
            .is_some(),
        "CONTROL: the wall is on record"
    );
    reader.answers.insert(work, usage("source", false));
    let ends = 960 + accounts::RESET_GRACE_S;
    let read = passes(&world, &reader, &mut pace, every_5s(365, ends + 5));
    assert_eq!(read, [(ends, both)], "{pace:?}");
}

/// THE FIRST ASK AT A LATER WALL READS: the own login's ladder is one series of asks'. A session
/// waits out a five-hour wall with `work` never taken, its ladder climbing to the 30 minute rung;
/// the wall resets, the session stops asking and works, and reaches its weekly limit while that
/// rung would still stand. Its first ask there reads its login, which puts the new wall on record.
#[test]
fn the_first_ask_at_a_later_wall_reads() {
    let (world, mut reader, main, work) = never_taken("pace-later-wall");
    let five_hour = usage_resetting("source", (100, 40), Some(NOW + 3200), NOW);
    reader.answers.insert(main.clone(), five_hour);
    let mut pace = ReadPace::new();
    let read = passes(&world, &reader, &mut pace, every_5s(0, 2200));
    assert_eq!(
        read,
        [0, 60, 360, 2160].map(|at| (at, vec![main.clone(), work.clone()])),
        "{pace:?}"
    );
    assert!(pace.holds(&main, true, NOW + 3400), "CONTROL: {pace:?}");
    reader.answers.insert(main.clone(), usage("source", true));
    let read = passes(&world, &reader, &mut pace, every_5s(3400, 3500));
    assert_eq!(read, [(3400, vec![main])], "{pace:?}");
    let walls = world.load(Cli::Codex).accounts["main"].walls.clone();
    assert!(walls.contains_key(accounts::WEEK_LIMIT), "{walls:?}");
}

/// A READ THAT PANICS STILL COUNTS: the pace settles when its pass is dropped, so a read that
/// unwinds is not made again at the next ask. A panic in the candidate's read leaves the row on
/// its first rung; one in the session's own login's read leaves that login on its ladder, read
/// again a minute later.
#[test]
fn a_read_that_panics_still_climbs() {
    for own in [false, true] {
        let (world, reader, main, work) = never_taken("pace-panicking-read");
        let panics = if own { &main } else { &work };
        let during = |_: &Seat<'_>, home: &Path| {
            assert!(home != panics.as_path(), "the read fails");
        };
        let hooked = Hooked {
            reader: &reader,
            during: &during,
        };
        let mut pace = ReadPace::new();
        let unwound = catch_unwind(AssertUnwindSafe(|| {
            let _ = ask(&world, &hooked, &mut pace, NOW);
        }));
        assert!(unwound.is_err(), "CONTROL {own}");
        let made = reader.calls.borrow().len();
        assert_eq!(made, if own { 1 } else { 2 }, "CONTROL {own}");
        let read = passes(&world, &reader, &mut pace, every_5s(5, 65));
        assert_eq!(
            read,
            [(60, vec![main.clone(), work.clone()])],
            "{own}: {pace:?}"
        );
    }
}

/// THE EXEC PATH IS PACED BY THE SAME GATE: a headless run's asks, `work` never taken, read by the
/// same rungs.
#[test]
#[cfg(unix)]
fn the_exec_path_is_paced_by_the_same_gate() {
    use crate::harness::codex_exec_handoff::ExecCommand;
    let (world, reader, main, work) = never_taken("pace-exec");
    let program = std::env::current_exe().unwrap();
    let image = crate::harness::native_act::Image::at(&program).unwrap();
    let asked = asked_session(&world);
    let mut pace = ReadPace::new();
    let mut read = Vec::new();
    for at in every_5s(0, HOUR) {
        let request = Session {
            cli: asked.cli,
            sid: asked.sid,
            supervisor: asked.supervisor,
            env: asked.env,
            seen: &crate::harness::observe::Observation::Unread,
            sample: asked.sample.clone(),
            point: codex_rotation::wall_asked(NOW + at),
            mode: asked.mode,
            relaunch: (),
        };
        let seat = Seat {
            now: NOW + at,
            ..world.seat()
        };
        let before = reader.calls.borrow().len();
        let proposal = codex_rotation::prepare_exec_paced(
            &seat,
            &request,
            &world.exe,
            &Identity::from_native("source", "user"),
            &reader,
            Duration::from_secs(60),
            &mut pace,
            |row| {
                Some(ExecCommand {
                    argv0: "codex".into(),
                    image: image.clone(),
                    program: program.to_str().unwrap().into(),
                    argv: vec!["exec".into()],
                    auth_path: codex_rotation::row_auth(&seat, row)
                        .to_str()
                        .unwrap()
                        .into(),
                    cwd: None,
                })
            },
        );
        assert!(matches!(proposal, Ok(None)), "{at}: {proposal:?}");
        if reader.calls.borrow().len() > before {
            read.push(at);
        }
    }
    assert_eq!(read, [0, 60, 360, 2160], "{pace:?}");
    assert_eq!(reader.calls.borrow().len(), 8);
    assert_eq!(reads_of(&reader, &main), reads_of(&reader, &work));
}

/// THE RUNGS ARE THE RECORDS' FIRST THREE, AND A RUN IS ONE EPISODE'S: 60 s, 5 minutes, then 30
/// minutes for every read after. A read six hours after the run's newest begins a new run. A
/// clock set back moves no run's newest back, so the login it holds stays held.
#[test]
fn a_run_climbs_the_records_first_three_rungs_and_ends_with_its_episode() {
    assert_eq!(PACE_S, accounts::UNANSWERED_S[..3]);
    let mut run = None;
    let mut waits = Vec::new();
    for _ in 0..5 {
        let next = Run::climbed(run, NOW);
        waits.push(next.stands_until() - NOW);
        run = Some(next);
    }
    assert_eq!(waits, [60, 300, 1800, 1800, 1800]);
    let later = NOW + accounts::UNANSWERED_RUN_S;
    assert_eq!(Run::climbed(run, later - 1).attempts, 6, "CONTROL");
    let anew = Run::climbed(run, later);
    assert_eq!((anew.attempts, anew.stands_until()), (1, later + 60));

    let back = Run::climbed(run, NOW - 100);
    assert_eq!((back.at, back.attempts), (NOW, 6));
    let (world, reader, ..) = never_taken("pace-clock-set-back");
    let mut pace = ReadPace::new();
    let read = passes(&world, &reader, &mut pace, [0, -100, -45, 55]);
    assert_eq!(read.iter().map(|(at, _)| *at).collect::<Vec<_>>(), [0]);
}

/// WHAT THE PACE LAYS IS NEVER WRITTEN AND SHORTENS NO RUN ON RECORD: the hold is laid over one
/// ask's copy of the records. A run already there that stands longer is kept as it is, and one
/// that stands less is laid over to the hold's end. An ask the pace refuses leaves the store's
/// state file byte for byte as it was.
#[test]
fn a_laid_hold_is_never_written_and_shortens_no_run_on_record() {
    let mut state = accounts::State::default();
    accounts::lay_unread_until(&mut state, "work", NOW + 300);
    assert_eq!(
        state.accounts["work"].unanswered_until(NOW),
        Some(NOW + 300)
    );
    accounts::lay_unread_until(&mut state, "work", NOW + 200);
    assert_eq!(
        state.accounts["work"].unanswered_until(NOW),
        Some(NOW + 300)
    );
    let longer = accounts::Unanswered {
        at: NOW,
        attempts: 4,
    };
    state.accounts.get_mut("work").unwrap().unanswered = Some(longer);
    accounts::lay_unread_until(&mut state, "work", NOW + 300);
    assert_eq!(state.accounts["work"].unanswered, Some(longer));
    accounts::lay_unread_until(&mut state, "work", longer.stands_until() + 1);
    assert_eq!(
        state.accounts["work"].unanswered_until(NOW),
        Some(longer.stands_until() + 1),
        "CONTROL"
    );

    let (world, reader, ..) = never_taken("pace-lays-nothing-on-disk");
    let mut pace = ReadPace::new();
    assert!(matches!(ask(&world, &reader, &mut pace, NOW), Ok(None)));
    let file = world.seat().store(Cli::Codex).state_file();
    let before = std::fs::read(&file).unwrap();
    let read = passes(&world, &reader, &mut pace, [5, 10]);
    assert!(read.is_empty(), "CONTROL: the pace held both asks");
    assert!(before == std::fs::read(&file).unwrap());
    let record = world.load(Cli::Codex).accounts.get("work").cloned();
    assert_eq!(record.and_then(|account| account.unanswered), None);
}

/// A PASS SETTLES INTO ONE HOLD PER LOGIN IT CLIMBED, AND A HOLD THAT REFUSES ASKS IS REPORTED
/// ONCE PER RUNG. A repeat pass climbs the own login (`row: None`) and each row it read; a pass
/// that read news climbs only its rows. The first ask a hold refuses reports it, the asks after
/// it report nothing, and the next climb's hold is reported again.
#[test]
fn a_pass_settles_into_its_holds_and_each_is_reported_once_per_rung() {
    let own = Path::new("/home/.codex/auth.json");
    let minted = |label: &str, news: bool| Minted {
        label: label.to_owned(),
        context: None,
        news,
        repeat: true,
        on_record: false,
    };
    let hold =
        |row: Option<&str>, attempts: u32, until: i64| (row.map(str::to_owned), attempts, until);
    let climbed = |climbs: Vec<Climb>| {
        climbs
            .into_iter()
            .map(|climb| (climb.row, climb.attempts, climb.until))
            .collect::<Vec<_>>()
    };
    let waiting = |waits: Vec<Wait>| {
        waits
            .into_iter()
            .map(|wait| (wait.row, wait.attempts, wait.until))
            .collect::<Vec<_>>()
    };
    let mut pace = ReadPace::new();
    let walled = OwnRead::Walled;
    let news = pace.settle(own, walled, None, vec![minted("work", true)], NOW);
    assert_eq!(climbed(news), [hold(Some("work"), 1, NOW + 60)]);
    assert_eq!(
        waiting(pace.waits(NOW + 5)),
        [hold(Some("work"), 1, NOW + 60)]
    );
    for at in (10..60).step_by(5) {
        assert!(pace.waits(NOW + at).is_empty(), "{at}");
    }
    assert!(pace.waits(NOW + 60).is_empty(), "no hold stands");
    let repeat = pace.settle(own, walled, None, vec![minted("work", false)], NOW + 60);
    assert_eq!(
        climbed(repeat),
        [hold(None, 1, NOW + 120), hold(Some("work"), 2, NOW + 360)]
    );
    assert_eq!(
        waiting(pace.waits(NOW + 65)),
        [hold(None, 1, NOW + 120), hold(Some("work"), 2, NOW + 360)]
    );
    assert!(pace.waits(NOW + 70).is_empty());
    assert!(
        pace.waits(NOW + 125).is_empty(),
        "the row's hold was reported"
    );
    let none = pace.settle(own, OwnRead::NoWall, None, Vec::new(), NOW + 400);
    assert_eq!(climbed(none), [], "a reading of no wall climbs nothing");
    assert_eq!(waiting(pace.waits(NOW + 405)), [hold(None, 0, NOW + 460)]);
}

/// THE OWN LOGIN'S LADDER ENDS WITH ITS SERIES OF ASKS, AND WITH ITS LOGIN FILE: an ask more than
/// [`ASKING_S`] after the one before ends it, an ask at that distance does not, and the rows'
/// runs stay either way. A ladder of one login file holds no ask of another. ONLY A LADDER THE
/// LOGIN'S LIMIT CLIMBED LETS NEWS PAST: one a read that ended its pass climbed, or one a reading
/// of no wall restarted, holds an ask for news too.
#[test]
fn the_own_ladder_ends_with_its_series_of_asks() {
    let own = Path::new("/home/.codex/auth.json");
    let mut pace = ReadPace::new();
    pace.asks(NOW);
    for _ in 0..2 {
        let _ = pace.settle(own, OwnRead::Walled, None, Vec::new(), NOW);
    }
    let named = Some(("work".to_owned(), None));
    let _ = pace.settle(own, OwnRead::Walled, named, Vec::new(), NOW);
    assert!(pace.holds(own, true, NOW + 1000), "CONTROL: {pace:?}");
    assert!(!pace.holds(own, false, NOW + 1000), "news: {pace:?}");
    assert!(!pace.holds(Path::new("/other/auth.json"), true, NOW + 1000));
    pace.asks(NOW + ASKING_S);
    assert!(pace.holds(own, true, NOW + 1000), "{pace:?}");
    pace.asks(NOW + 2 * ASKING_S + 1);
    assert!(!pace.holds(own, true, NOW + 1000), "{pace:?}");
    assert!(pace.asked("work"), "{pace:?}");

    for (ended, until) in [(OwnRead::Ended, 60), (OwnRead::NoWall, 60)] {
        let mut pace = ReadPace::new();
        let named = Some(("work".to_owned(), None));
        let _ = pace.settle(own, ended, named, Vec::new(), NOW);
        assert!(pace.holds(own, false, NOW + until - 1), "{ended:?}");
        assert!(!pace.holds(own, false, NOW + until), "{ended:?}");
        assert!(!pace.asked("work"), "{ended:?}: it was never reached");
    }
}

/// Asks every five seconds over `from..to`, as a control answers them: an ask that brings a
/// selection is followed by those of the next `hold` seconds, which the control answers from that
/// selection with no pass, each noted to the pace ([`ReadPace::asks`]) only where `fed`; the
/// offsets of the asks that read.
fn asks_through_holds(
    world: &World,
    reader: &Reader,
    pace: &mut ReadPace,
    (from, to): (i64, i64),
    hold: i64,
    fed: bool,
) -> Vec<i64> {
    let mut held_to = None;
    let mut read = Vec::new();
    for at in every_5s(from, to) {
        if held_to.is_some_and(|end| at <= end) {
            if fed {
                pace.asks(NOW + at);
            }
            continue;
        }
        let before = reader.calls.borrow().len();
        let asked = ask(world, reader, pace, NOW + at);
        if reader.calls.borrow().len() > before {
            read.push(at);
        }
        if matches!(asked, Ok(Some(_))) {
            held_to = Some(at + hold);
        }
    }
    read
}

/// A CONTROL THAT NOTES EVERY ASK IT ANSWERS KEEPS THE OWN LOGIN'S LADDER THROUGH ITS HOLDS: five
/// rows each read usable on an account of its own, so each pass that reads one selects it, and
/// none is ever taken. The control holds each selection for a minute (the TUI's observation),
/// answering the asks of that minute with no pass, and the ask after it reads. Noted, those asks
/// are one series with the passes, and an hour reads the session's login nine times: once with
/// each row it had not read, and four times on its own ladder. CONTROL: unnoted, a minute and five
/// seconds pass between two passes, each of them ends the ladder, and the hour reads that login
/// twenty times.
#[test]
fn a_control_that_notes_every_ask_keeps_the_own_ladder_through_its_holds() {
    let labels = ["t1", "t2", "t3", "t4", "t5"];
    let mut source_reads = Vec::new();
    for fed in [true, false] {
        let (world, mut reader, files) = pool("pace-held-selections", &labels);
        for (file, label) in files.iter().zip(labels) {
            reader.answers.insert(file.clone(), usage(label, false));
        }
        let mut pace = ReadPace::new();
        let read = asks_through_holds(&world, &reader, &mut pace, (0, HOUR), 60, fed);
        if fed {
            assert_eq!(
                read,
                [0, 65, 130, 195, 260, 325, 390, 495, 2295],
                "{pace:?}"
            );
        }
        source_reads.push(reads_of(&reader, &main_of(&world)));
    }
    assert_eq!(source_reads, [9, 20], "fed, then CONTROL unfed");
}

/// A CLOCK SET BACK ENDS A SERIES OF ASKS: how long the process did not ask is unknown, so the
/// own login's ladder ends with it and the first ask after reads. The session's own login answers
/// as another identity, so each pass reads it alone and climbs its ladder: passes at 0, 60 and
/// 360 s leave it standing to 2160 s. With the clock then an hour back, the next ask reads at
/// once and the ladder climbs from its first rung again, the ask a minute later reading next.
/// CONTROL: the ladder held an ask on the clock as it was.
#[test]
fn a_clock_set_back_ends_the_series_of_asks() {
    let (world, mut reader) = fixture("pace-clock-set-back-series");
    let main = main_of(&world);
    reader.answers.insert(main.clone(), usage("another", true));
    let mut pace = ReadPace::new();
    let read = passes(&world, &reader, &mut pace, every_5s(0, 400));
    assert_eq!(
        read,
        [0, 60, 360].map(|at| (at, vec![main.clone()])),
        "{pace:?}"
    );
    assert!(pace.holds(&main, false, NOW + 400), "CONTROL: {pace:?}");
    let back = 400 - HOUR;
    let read = passes(&world, &reader, &mut pace, every_5s(back, back + 100));
    assert_eq!(
        read,
        [back, back + 60].map(|at| (at, vec![main.clone()])),
        "{pace:?}"
    );
}

/// A ROW READ WALLED ON ANOTHER LIMIT ON RECORD IS READ WHEN THAT WALL ENDS, AS A ROW WALLED ON THE
/// CODEX BUCKET IS: a session on `main` asks about `codex_other`, `main` spent on it; `work` lists
/// it with room but is never taken (it reads on the session's own account), so it is read at 0
/// and 60 s, and the read at 360 s answers `codex_other` spent, resetting at 960 s. That wall goes
/// on record as the limit's, which gates the row from there; the first ask after it and its grace
/// reads `work` again, not the one at 2160 s its third rung ends at.
#[test]
fn a_row_read_walled_on_another_limit_on_record_is_read_when_that_wall_ends() {
    let (world, mut reader, main, work) = never_taken("pace-limit-wall-ends-on-record");
    reader
        .answers
        .insert(main.clone(), read_with_limit("source", (10, 10), Some(100)));
    reader
        .answers
        .insert(work.clone(), read_with_limit("source", (10, 10), Some(30)));
    let source = Identity::from_native("source", "user");
    let mut pace = ReadPace::new();
    let asks = |reader: &Reader, pace: &mut ReadPace, offsets: &mut dyn Iterator<Item = i64>| {
        let mut read = Vec::new();
        for offset in offsets {
            let before = reader.calls.borrow().len();
            let seat = Seat {
                now: NOW + offset,
                ..world.seat()
            };
            let mut session = asked_session(&world);
            session.point = codex_rotation::wall_asked(NOW + offset);
            let _ = codex_rotation::prepare_paced(
                &seat,
                &session,
                codex_rotation::Ask::of(&source, Some(OTHER)),
                reader,
                Duration::from_secs(60),
                pace,
            );
            let made = reader.calls.borrow()[before..].to_vec();
            if !made.is_empty() {
                read.push((offset, made));
            }
        }
        read
    };
    let both = vec![main, work.clone()];
    let read = asks(&reader, &mut pace, &mut every_5s(0, 360));
    assert_eq!(read, [0, 60].map(|at| (at, both.clone())), "{pace:?}");
    let mut spent = read_with_limit("source", (10, 10), Some(100));
    let entry = spent.usage.limits.get_mut(OTHER).expect("the limit");
    entry.windows[0].reset = Some(NOW + 960);
    reader.answers.insert(work.clone(), spent);
    let read = asks(&reader, &mut pace, &mut [360].into_iter());
    assert_eq!(read, [(360, both.clone())]);
    assert!(
        world.load(Cli::Codex).accounts["work"]
            .limit_wall_until(OTHER, NOW + 360)
            .is_some(),
        "CONTROL: the limit's wall is on record"
    );
    reader
        .answers
        .insert(work, read_with_limit("source", (10, 10), Some(30)));
    let ends = 960 + accounts::RESET_GRACE_S;
    let read = asks(&reader, &mut pace, &mut every_5s(365, ends + 5));
    assert_eq!(read, [(ends, both)], "{pace:?}");
}
