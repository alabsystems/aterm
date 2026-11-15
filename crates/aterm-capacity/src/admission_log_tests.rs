// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The admission log's rows ([`super`]): a memory wait's `look` rows, the admitting look's, one
//! row a minute for a wait whatever waiter values look, a forced admission's admitting look, the
//! held claims of memory a look beside a pressure shortfall names, the `admitted` row's kinds of
//! shortfall, no row for a scratch ledger or a wait short of no memory, the one rule of who
//! writes, the account's folder, an admission that succeeds whatever its row's write does, and
//! the log read back across its rotated files. Every ledger is a scratch one on a [`Fixed`]
//! machine; the user ledger each test stands in is its own ([`super::capture`]), so no row
//! reaches this account's folder.

use std::cell::Cell;
use std::path::PathBuf;

use aterm_json::Value;

use super::{Row, Waits, capture, capture_in, id_of, read};
use crate::{Admit, Fixed, Forced, Ledger, Need, Pace, Polled, Reading, Shortfall, Waiter};

const GIB: u64 = 1 << 30;

/// A scratch folder and the ledger in it.
fn scratch(label: &str) -> (aterm_tempfile::TempDir, Ledger) {
    let dir = aterm_tempfile::Builder::new()
        .prefix(&format!("cap-log-{label}-"))
        .tempdir()
        .expect("a scratch folder");
    let ledger = Ledger::at(dir.path().join("ledger"));
    (dir, ledger)
}

/// One look by `w`, which waits.
#[track_caller]
fn waits(w: Waiter, m: &Fixed) -> (Waiter, Shortfall) {
    match w.poll(m) {
        Polled::Waiting(w, short) => (w, short),
        Polled::Admitted(c) => panic!("expected a wait, admitted: {:?}", c.record()),
        Polled::Gone(e) => panic!("expected a wait, gone: {e}"),
        Polled::Unqualified(why) => panic!("expected a wait, unqualified: {why}"),
    }
}

/// One look by `w`, which admits it.
#[track_caller]
fn admitted(w: Waiter, m: &Fixed) -> crate::Claim {
    match w.poll(m) {
        Polled::Admitted(c) => c,
        Polled::Waiting(_, short) => panic!("expected admission, waits: {short}"),
        Polled::Gone(e) => panic!("expected admission, gone: {e}"),
        Polled::Unqualified(why) => panic!("expected admission, unqualified: {why}"),
    }
}

/// The text values of `row`'s array field `key`.
fn words(row: &Row, key: &str) -> Vec<String> {
    row.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The objects of `row`'s array field `key`.
fn objects<'r>(row: &'r Row, key: &str) -> &'r [Value] {
    row.get(key)
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
}

/// The `look` rows among `rows`.
fn looks(rows: &[Row]) -> Vec<&Row> {
    rows.iter().filter(|r| r.kind() == "look").collect()
}

/// A memory-short waiter behind one held 10 GiB claim looks twice within the minute: one `look`
/// row, which names its claim, what the look read available and what the claims ahead reserve,
/// the holder by its id with what it claims and still reserves, and the same holder among every
/// held claim of memory; once the holder ends, the look that admits it writes the admitting row.
/// NEGATIVE CONTROL: a wait short only of cores writes none.
#[test]
fn a_look_row_names_each_holder_and_the_room() {
    let (_dir, ledger) = scratch("look");
    let m = Fixed::new(Reading::of_gib(32, 15, 14));
    let holder = admitted(
        Waiter::enter(&ledger, &m, Need::new("verify test compile").mem_gib(10)).expect("entered"),
        &m,
    );
    let ((waiter_id, short), rows) = capture(ledger.clone(), || {
        let w = Waiter::enter(&ledger, &m, Need::new("verify tippy lint").mem_gib(12))
            .expect("entered");
        let id = id_of(w.record().order());
        let (w, short) = waits(w, &m);
        let (w, again) = waits(w, &m);
        assert_eq!(short.shorts, again.shorts, "both looks are short alike");
        drop(holder);
        drop(admitted(w, &m));
        (id, short)
    });
    assert!(
        short
            .shorts
            .iter()
            .any(|s| matches!(s, crate::Short::Memory { .. })),
        "the fixture waits for memory: {short}"
    );
    let looks = looks(&rows);
    assert_eq!(
        looks.len(),
        2,
        "one row for both waiting looks, one admitting: {rows:?}"
    );
    let row = looks[0];
    assert_eq!(row.str("id"), Some(waiter_id.as_str()));
    assert_eq!(
        row.get("claim"),
        None,
        "one waiter: its own id names the wait"
    );
    assert_eq!(row.u64("mem"), Some(12 * GIB));
    assert!(!row.flag("admitted"));
    assert_eq!(words(row, "shorts"), ["memory"]);
    assert_eq!(row.u64("available"), Some(15 * GIB));
    let Some(crate::Short::Memory { reserved, .. }) = short.shorts.first() else {
        panic!("memory first: {short}");
    };
    assert_eq!(row.u64("reserved"), Some(*reserved));
    let holders = objects(row, "holders");
    assert_eq!(holders.len(), 1, "{holders:?}");
    let h = &short.holders[0];
    assert_eq!(
        holders[0].get("id").and_then(Value::as_str),
        Some(id_of(h.order).as_str())
    );
    assert_eq!(holders[0].get("held").and_then(Value::as_bool), Some(true));
    assert_eq!(
        holders[0].get("claimed").and_then(Value::as_u64),
        Some(10 * GIB)
    );
    assert_eq!(
        holders[0].get("reserved").and_then(Value::as_u64),
        Some(h.mem)
    );
    let held = objects(row, "held_mem");
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(
        held[0].get("id").and_then(Value::as_str),
        Some(id_of(h.order).as_str())
    );
    assert_eq!(held[0].get("reserved").and_then(Value::as_u64), Some(h.mem));
    let admitting = looks[1];
    assert_eq!(admitting.str("id"), Some(waiter_id.as_str()));
    assert!(admitting.flag("admitted"));
    assert!(words(admitting, "shorts").is_empty());

    // NEGATIVE CONTROL: an earlier waiter for every core keeps them, so a later one is short of
    // cores alone, and its looks write nothing.
    let ((), rows) = capture(ledger.clone(), || {
        let ahead = Waiter::enter(&ledger, &m, Need::new("cores ahead").cores(14)).expect("ahead");
        let w = Waiter::enter(&ledger, &m, Need::new("verify test subprocesses").cores(14))
            .expect("entered");
        let (w, short) = waits(w, &m);
        assert!(
            short
                .shorts
                .iter()
                .all(|s| matches!(s, crate::Short::Cores)),
            "short of cores alone: {short}"
        );
        drop(waits(w, &m));
        drop(ahead);
    });
    assert!(rows.is_empty(), "no memory look, no row: {rows:?}");
}

/// A caller that drops its waiter after each look and enters a fresh one for the next (a
/// compiler unit's wait, a test binary that is not the dispatch's queued one) waits one wait:
/// three looks within the minute, each by a new claim, write one `look` row, and the fresh
/// waiter whose look admits it writes the admitting row under the wait's first id, naming its own
/// claim beside it. NEGATIVE CONTROL: a waiter kept across its looks writes no `claim`, above.
#[test]
fn a_fresh_waiter_at_each_look_writes_one_row_a_minute_under_one_id() {
    let (_dir, ledger) = scratch("fresh");
    let m = Fixed::new(Reading::of_gib(32, 15, 14));
    let holder = admitted(
        Waiter::enter(&ledger, &m, Need::new("held").mem_gib(10)).expect("entered"),
        &m,
    );
    let need = || Need::new("verify test binary").mem_gib(12);
    let ((ids, last), rows) = capture(ledger.clone(), || {
        let mut ids = Vec::new();
        for _ in 0..3 {
            let w = Waiter::enter(&ledger, &m, need()).expect("entered");
            ids.push(id_of(w.record().order()));
            drop(waits(w, &m));
        }
        drop(holder);
        let w = Waiter::enter(&ledger, &m, need()).expect("entered");
        let last = id_of(w.record().order());
        drop(admitted(w, &m));
        (ids, last)
    });
    assert_eq!(ids.len(), 3);
    assert!(
        ids[0] != ids[1] && ids[1] != ids[2],
        "each look a new claim: {ids:?}"
    );
    let looks = looks(&rows);
    assert_eq!(looks.len(), 2, "one waiting row, one admitting: {rows:?}");
    assert_eq!(looks[0].str("id"), Some(ids[0].as_str()));
    assert!(!looks[0].flag("admitted"));
    assert_eq!(
        looks[1].str("id"),
        Some(ids[0].as_str()),
        "the wait's first id"
    );
    assert!(looks[1].flag("admitted"));
    assert_eq!(
        looks[1].str("claim"),
        Some(last.as_str()),
        "the claim admitted"
    );
}

/// The waits' clock, with exact times: a wait's first memory look is due a row under the claim
/// that looked; a fresh claim's look within the minute is not, and at the minute it is, under the
/// first id; a look short of no memory keeps the wait open and writes none; the admitting look
/// ends it. NEGATIVE CONTROLS: a look short of no memory opens no wait, another need is another
/// wait, and a wait no look has seen for [`super::SILENT_ENDS`] is over, so the next opens anew.
#[test]
fn the_waits_say_a_row_a_minute_and_end_at_admission_or_silence() {
    let key = |what: &str| super::WaitKey {
        ledger: PathBuf::from("/scratch/ledger"),
        what: what.to_owned(),
        mem: 4 * GIB,
    };
    let unit = key("verify test compile unit");
    let mut waits = Waits::default();
    assert_eq!(waits.look(&unit, "a", 1_000, true).as_deref(), Some("a"));
    assert_eq!(waits.look(&unit, "b", 31_000, true), None);
    assert_eq!(waits.look(&unit, "c", 60_999, true), None);
    assert_eq!(waits.look(&unit, "d", 61_000, true).as_deref(), Some("a"));
    assert_eq!(waits.look(&unit, "e", 90_000, false), None);
    assert!(waits.open(&unit, 90_000), "a look short of cores keeps it");
    assert_eq!(waits.admitted(&unit, 91_000).as_deref(), Some("a"));
    assert!(!waits.open(&unit, 91_000), "the admitting look ends it");
    assert_eq!(waits.admitted(&unit, 91_000), None);

    assert_eq!(waits.look(&unit, "f", 92_000, false), None);
    assert!(!waits.open(&unit, 92_000), "no memory look, no wait");
    let other = key("verify test binary");
    assert_eq!(waits.look(&unit, "g", 93_000, true).as_deref(), Some("g"));
    assert_eq!(waits.look(&other, "h", 93_500, true).as_deref(), Some("h"));
    assert!(waits.open(&unit, 93_000 + super::SILENT_ENDS - 1));
    assert!(
        !waits.open(&unit, 93_000 + super::SILENT_ENDS),
        "silent too long"
    );
    assert_eq!(
        waits
            .look(&unit, "i", 93_000 + super::SILENT_ENDS, true)
            .as_deref(),
        Some("i"),
        "a new wait"
    );
}

/// A forced admission (`--admit now`) of a wait that wrote a `look` row writes its admitting row,
/// as a look's admission does, and its claim says it was forced.
#[test]
fn a_forced_admission_writes_its_admitting_look() {
    let (_dir, ledger) = scratch("forced");
    let m = Fixed::new(Reading::of_gib(32, 15, 14));
    let holder = admitted(
        Waiter::enter(&ledger, &m, Need::new("held").mem_gib(10)).expect("entered"),
        &m,
    );
    let ((id, forced), rows) = capture(ledger.clone(), || {
        let w = Waiter::enter(&ledger, &m, Need::new("forced").mem_gib(12)).expect("entered");
        let id = id_of(w.record().order());
        let (w, _) = waits(w, &m);
        let claim = match w.force(&m).expect("a forced look") {
            Forced::Admitted(claim, would) => {
                assert!(would.is_some(), "it would have waited for memory");
                claim
            }
            Forced::Turn(_, short) => panic!("expected admission, handed back: {short}"),
        };
        (id, claim.forced())
    });
    drop(holder);
    assert!(forced, "its claim says --admit now admitted it");
    let looks = looks(&rows);
    assert_eq!(
        looks.len(),
        2,
        "its waiting row and its admitting row: {rows:?}"
    );
    assert!(!looks[0].flag("admitted"));
    assert_eq!(looks[1].str("id"), Some(id.as_str()));
    assert!(looks[1].flag("admitted"));
}

/// A look short only of the kernel's pressure, with memory beside every reservation, names no
/// holder in its shortfall, but its row lists every held claim of memory it was held to, with
/// what each claims and still reserves, so `room*` counts them all.
#[test]
fn a_pressure_look_lists_every_held_claim_of_memory() {
    let (_dir, ledger) = scratch("pressure");
    let m =
        Fixed::new(Reading::of_gib(64, 40, 14).at_pressure(aterm_sysprobe::MemoryLevel::Critical));
    let calm = Fixed::new(Reading::of_gib(64, 40, 14));
    let holder = admitted(
        Waiter::enter(&ledger, &calm, Need::new("held").mem_gib(10)).expect("entered"),
        &calm,
    );
    let held_id = id_of(holder.record().order());
    let (short, rows) = capture(ledger.clone(), || {
        let w = Waiter::enter(&ledger, &m, Need::new("waits").mem_gib(4)).expect("entered");
        waits(w, &m).1
    });
    drop(holder);
    assert_eq!(short.shorts, [crate::Short::Pressure], "{short}");
    assert!(
        short.holders.is_empty(),
        "pressure names no holder: {short}"
    );
    let looks = looks(&rows);
    assert_eq!(looks.len(), 1, "{rows:?}");
    assert!(objects(looks[0], "holders").is_empty());
    let held = objects(looks[0], "held_mem");
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(
        held[0].get("id").and_then(Value::as_str),
        Some(held_id.as_str())
    );
    assert_eq!(
        held[0].get("claimed").and_then(Value::as_u64),
        Some(10 * GIB)
    );
    assert_eq!(
        held[0].get("reserved").and_then(Value::as_u64),
        Some(10 * GIB),
        "no tree on a fixed machine uses any of it"
    );
}

/// An admission a caller decides itself and gives an `Asked` its decision (atpkg's host job's
/// one look, a nested build's look in its turn) writes its `admitted` row, which says every kind
/// of shortfall its looks found: here a look that met another admission's lock. NEGATIVE
/// CONTROL: the same decision in another ledger than the user's writes none.
#[test]
fn an_asked_admission_writes_its_admitted_row() {
    let (_dir, ledger) = scratch("asked");
    let (_other_dir, other) = scratch("asked-other");
    let m = Fixed::new(Reading::of_gib(32, 15, 14));
    let need = Need::new("targo build").mem_gib(2);
    let digest = aterm_telemetry::Digest::of("test", &[b"facts"]);
    let (id, rows) = capture(ledger.clone(), || {
        let asked = crate::telemetry::Asked::of(&need);
        let w = Waiter::enter(&ledger, &m, need.clone()).expect("entered");
        let turn = ledger.hold_turn().expect("the lock").expect("free");
        let (w, short) = waits(w, &m);
        assert!(
            matches!(short.shorts[..], [crate::Short::Turn { .. }]),
            "met the lock: {short}"
        );
        drop(turn);
        let claim = admitted(w, &m);
        let id = id_of(claim.record().order());
        asked.decided_in(&ledger, ledger.dir(), "m8", digest, &Ok(claim), |_| true);
        id
    });
    let decisions: Vec<&Row> = rows.iter().filter(|r| r.kind() == "admitted").collect();
    assert_eq!(decisions.len(), 1, "{rows:?}");
    assert_eq!(decisions[0].str("id"), Some(id.as_str()));
    assert_eq!(words(decisions[0], "shorts"), ["turn"]);
    assert!(!decisions[0].flag("forced"));

    let ((), rows) = capture(ledger.clone(), || {
        let w = Waiter::enter(&other, &m, need.clone()).expect("entered");
        let claim = admitted(w, &m);
        crate::telemetry::Asked::of(&need).decided_in(
            &other,
            other.dir(),
            "m8",
            digest,
            &Ok(claim),
            |_| true,
        );
    });
    assert!(rows.is_empty(), "another ledger's admission: {rows:?}");
}

/// A scratch ledger's admissions write no row: the capture stands in another ledger for this
/// user's, and a memory wait, its admission and an `admit_with` decision in the scratch one write
/// nothing.
#[test]
fn a_scratch_ledger_writes_no_admission_log() {
    let (_dir, ledger) = scratch("scratch");
    let (_user_dir, user) = scratch("user");
    let m = Fixed::new(Reading::of_gib(32, 15, 14));
    let ((), rows) = capture(user, || {
        let holder = admitted(
            Waiter::enter(&ledger, &m, Need::new("held").mem_gib(10)).expect("entered"),
            &m,
        );
        let (w, _) = waits(
            Waiter::enter(&ledger, &m, Need::new("waits").mem_gib(12)).expect("entered"),
            &m,
        );
        drop(holder);
        drop(admitted(w, &m));
        drop(
            crate::admit_with(
                &ledger,
                &m,
                Need::new("decided").mem_gib(1),
                Admit::Wait,
                Pace::default(),
                &mut |_| {},
            )
            .expect("admitted"),
        );
    });
    assert!(rows.is_empty(), "{rows:?}");
}

/// The one rule of who writes, production's and a capture's alike: another ledger than the
/// account's writes no row, and asks for no writer; the account's own ledger, named by another
/// value of the same folder, gets the log's writer.
#[test]
fn only_the_accounts_own_ledger_writes_rows() {
    let (_dir, user) = scratch("own");
    let (_other_dir, other) = scratch("not-own");
    let asked = Cell::new(0_u32);
    let log = || {
        asked.set(asked.get() + 1);
        Some(super::Writer::at(PathBuf::from("/nowhere/admission-log")))
    };
    assert!(super::writer_in(&other, &user, log).is_none());
    assert_eq!(asked.get(), 0, "a scratch ledger asks for no writer");
    assert!(super::writer_in(&Ledger::at(user.dir()), &user, log).is_some());
    assert_eq!(asked.get(), 1);
}

/// The log's folder follows the home the account database names, as the ledger does, not
/// `$HOME`: given a home, it is that home's state folder, read from nothing else; with none
/// there is none. A test binary runs under its scratch root, which keeps its log: never
/// the account's own folder.
#[test]
fn the_log_follows_the_account_home() {
    let home = PathBuf::from("/Users//another-home");
    let dir = super::account_dir(Some(home.clone())).expect("a folder");
    assert!(dir.starts_with(&home), "{}", dir.display());
    assert!(dir.ends_with(super::DIR), "{}", dir.display());
    assert_eq!(super::account_dir(None), None);
    #[cfg(unix)]
    {
        assert!(aterm_types::dirs::runs_under_private_state_root());
        assert_ne!(
            super::default_dir(),
            super::account_dir(aterm_uds::account::home()),
            "a test binary never logs to the account's folder"
        );
    }
}

/// A row's write that fails never touches its admission: with the log's folder a regular file,
/// so no row and no loss record can be made in it, a memory wait's looks and its `admit_with`
/// decision still admit, and each row is lost and counted. NEGATIVE CONTROL: a mutant that
/// propagated the failed write would refuse or end the admission this asserts.
#[test]
fn an_unwritable_log_never_refuses_admission() {
    let (dir, ledger) = scratch("unwritable");
    let blocked = dir.path().join("admission-log");
    std::fs::write(&blocked, b"a file cannot hold the admission log").expect("the blocker");
    let m = Fixed::new(Reading::of_gib(32, 15, 14));
    let before = super::lost();
    capture_in(ledger.clone(), blocked.clone(), || {
        let holder = admitted(
            Waiter::enter(&ledger, &m, Need::new("held").mem_gib(10)).expect("entered"),
            &m,
        );
        let (w, _) = waits(
            Waiter::enter(&ledger, &m, Need::new("waits").mem_gib(12)).expect("entered"),
            &m,
        );
        drop(holder);
        drop(admitted(w, &m));
        let claim = crate::admit_with(
            &ledger,
            &m,
            Need::new("decided").mem_gib(1),
            Admit::Wait,
            Pace::default(),
            &mut |_| {},
        );
        assert!(claim.is_ok(), "admitted whatever its row did: {claim:?}");
    });
    // The waiting look's row, the admitting look's and the decision's: each lost, none written.
    assert!(
        super::lost() >= before + 3,
        "three rows counted lost: {} then {}",
        before,
        super::lost()
    );
    assert!(blocked.is_file(), "the folder was never made over the file");
}

/// The log reads back what a writer appended: each row whole, the file's host line dating the
/// log, and a line that is no row counted torn.
#[test]
fn the_log_reads_back_its_rows() {
    let (dir, _) = scratch("read");
    let log = dir.path().join("admission-log");
    let writer = super::Writer::at(log.clone());
    assert!(writer.write(&Row::new("run", 1_000).with("checkout", "/ck")));
    assert!(writer.write(&Row::new("stage", 2_000).with("peak", 5 * GIB)));
    let current = aterm_telemetry::log::file_path(&log, 0);
    let mut text = std::fs::read_to_string(&current).expect("written");
    text.push_str("{\"v\":1,\"kind\":\"cut\n");
    std::fs::write(&current, text).expect("torn");
    let all = read(&log, None).expect("read");
    let kinds: Vec<&str> = all.rows.iter().map(Row::kind).collect();
    assert_eq!(kinds, ["run", "stage"]);
    assert_eq!(all.rows[1].u64("peak"), Some(5 * GIB));
    assert_eq!(all.torn, 1);
    assert_eq!(all.files, 1);
    let since = read(&log, Some(1_500)).expect("read");
    assert_eq!(since.rows.len(), 1, "{since:?}");
}

/// The log reads every rotated file, the oldest (highest-numbered) first: rows come back oldest
/// first across three files; a start inside the middle file cuts across the boundary and keeps
/// every later file's rows; how far back the log and each kind reach is read whatever the start;
/// another schema's row and a torn last line with no line feed count torn. A missing folder is a
/// log of no files.
#[test]
fn the_log_reads_every_rotated_file_oldest_first() {
    let (dir, _) = scratch("rotated");
    let log = dir.path().join("admission-log");
    std::fs::create_dir_all(&log).expect("the folder");
    let file = |n: u32, lines: &[String]| {
        std::fs::write(aterm_telemetry::log::file_path(&log, n), lines.concat()).expect("written");
    };
    file(
        2,
        &[
            Row::new("run", 1_000).line(),
            Row::new("stage", 2_000).line(),
        ],
    );
    file(
        1,
        &[
            Row::new("stage", 3_000).line(),
            "{\"v\":2,\"kind\":\"look\",\"at_ms\":3500}\n".to_owned(),
            Row::new("look", 4_000).line(),
        ],
    );
    file(
        0,
        &[
            Row::new("admitted", 5_000).line(),
            "{\"v\":1,\"kind\":\"cut".to_owned(),
        ],
    );
    let all = read(&log, None).expect("read");
    assert_eq!(all.files, 3);
    let at: Vec<(&str, u64)> = all.rows.iter().map(|r| (r.kind(), r.at_ms())).collect();
    assert_eq!(
        at,
        [
            ("run", 1_000),
            ("stage", 2_000),
            ("stage", 3_000),
            ("look", 4_000),
            ("admitted", 5_000)
        ]
    );
    assert_eq!(all.torn, 2, "the v2 row and the torn last line");
    assert_eq!(all.oldest_ms, Some(1_000));

    let since = read(&log, Some(3_500)).expect("read");
    let at: Vec<u64> = since.rows.iter().map(Row::at_ms).collect();
    assert_eq!(at, [4_000, 5_000], "cut inside the middle file");
    assert_eq!(
        since.oldest_ms,
        Some(1_000),
        "the log's reach, whatever the start"
    );
    assert_eq!(since.oldest_by_kind.get("run"), Some(&1_000));
    assert_eq!(since.oldest_by_kind.get("stage"), Some(&2_000));

    let none = read(&dir.path().join("no-such-log"), None).expect("a missing folder reads");
    assert_eq!(none.files, 0);
    assert!(none.rows.is_empty());
}

/// The judgment turn's shortfall has a word and a bit of its own, so a waiter's `admitted` row
/// names it among what its looks found.
/// NEGATIVE CONTROL: the held turn's word and bit are not the judgment turn's.
#[test]
fn the_judgment_turn_has_its_own_word_and_bit() {
    let heavy = crate::Short::Heavy {
        pid: 7,
        what: "verify land".to_owned(),
        held: true,
        predicted: std::time::Duration::from_secs(600),
        queued: std::time::Duration::from_secs(30),
    };
    let turn = crate::Short::Turn { holder: None };
    assert_eq!(super::short_word(&heavy), "heavy");
    assert_ne!(super::bit(&heavy), 0, "a word outside WORDS has no bit");
    assert_ne!(super::bit(&heavy), super::bit(&turn));
    assert_eq!(super::short_word(&turn), "turn");
}
