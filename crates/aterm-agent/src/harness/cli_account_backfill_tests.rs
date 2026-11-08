// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The account backfill in `aterm harness accounts history` ([`crate::harness::account_backfill`]):
//! a one-login home's transcript walls from before its series, booked in memory as closes and
//! marked `(backfill)`; nothing under several logins; nothing from before another account was
//! seen on the home or a `/login`; a read cut by its budget; no transcript text kept; nothing
//! written. Every transcript here is synthetic.

use super::*;
use crate::harness::account_backfill::{self, Cut};
use crate::harness::account_series::{self, Fig, Said, SeriesRow};
use crate::harness::spend::Limits;

const H: i64 = 3600;
const D: i64 = 86_400;
const SPAN: i64 = 30 * D;
/// A word no booked row, printed line or JSON value may hold: the wall rows carry it in every
/// field a transcript row has text in.
const SENTINEL: &str = "sentinel_6e2b";

/// UTC: the zone the tests read local days in.
fn utc(_: i64) -> i64 {
    0
}

/// `env_at`, naming the default Claude Code home (`~/.claude`, its profile `~/.claude.json`).
fn home_env(tmp: &Tmp) -> Env {
    let mut env = env_at(tmp);
    env.claude_profile = env.home.as_ref().map(|home| home.join(".claude.json"));
    env
}

/// A roster on `env` of `work`, the default home's login, and the rows `others` (each with its
/// own login), every one with a record (`work`'s keyed to another account at `keyed_at`, where
/// that is given); the state's machine id.
fn roster(tmp: &Tmp, env: &Env, others: &[&str], keyed_at: Option<i64>) -> String {
    let mut text = accounts::append_row("", "work", Class::Preferred, None).expect("work admits");
    for label in others {
        let dir = tmp.path().join("logins").join(label);
        login_made_in(&dir);
        text = accounts::append_row(&text, label, Class::Preferred, dir.to_str())
            .expect("the row admits");
    }
    std::fs::write(env.accounts_roster().expect("a config dir"), text).expect("write");
    let mut note = None;
    env.accounts_store()
        .transact(env.now, &mut note, |state| {
            state
                .accounts
                .entry("work".to_owned())
                .or_default()
                .keyed_at = keyed_at;
            for label in others {
                state.accounts.entry((*label).to_owned()).or_default();
            }
            Ok(Vec::new())
        })
        .expect("state");
    env.accounts_store().load(env.now).expect("load").0.machine
}

fn reading(at: i64, five: Option<(u32, i64)>, week: Option<(u32, i64)>) -> SeriesRow {
    let fig = |(pct_x10, reset): (u32, i64)| Fig {
        pct_x10,
        reset,
        at,
        src: accounts::SOURCE_TAP.into(),
        epoch: None,
    };
    SeriesRow {
        t: at,
        by: "m".into(),
        said: Said::Fig {
            five_hour: five.map(fig),
            seven_day: week.map(fig),
            ac: None,
            key: None,
        },
    }
}

fn write_series(env: &Env, machine: &str, rows: &[SeriesRow]) -> PathBuf {
    let path = account_series::path_of(&env.state, "work", machine).expect("a series path");
    std::fs::create_dir_all(path.parent().expect("its directory")).expect("mkdir");
    let text: String = rows
        .iter()
        .map(|r| r.line().expect("a row") + "\n")
        .collect();
    std::fs::write(&path, text).expect("write");
    path
}

/// A rejected quota row of window `w` at `at` naming the reset `reset`, with the sentinel in its
/// ids, its working directory, its notice and its credits word.
fn wall_row(at: i64, w: &str, reset: i64) -> String {
    format!(
        r#"{{"parentUuid":"{SENTINEL}-parent","isSidechain":false,"type":"assistant","uuid":"{SENTINEL}-uuid","sessionId":"{SENTINEL}-session","cwd":"/{SENTINEL}/cwd","timestamp":"{}","message":{{"id":"{SENTINEL}-msg","model":"<synthetic>","role":"assistant","type":"message","usage":{{"input_tokens":0,"output_tokens":0}},"content":[{{"type":"text","text":"You've hit your session limit · {SENTINEL} notice"}}]}},"requestId":"{SENTINEL}-req","quotaLimits":{{"status":"rejected","resetsAt":{reset},"unifiedRateLimitFallbackAvailable":false,"rateLimitType":"{w}","overageStatus":"rejected","overageDisabledReason":"{SENTINEL}","isUsingOverage":false}},"error":"rate_limit","isApiErrorMessage":true,"apiErrorStatus":429}}"#,
        rfc3339_utc(at)
    )
}

/// A `/login` that succeeded, at `at`.
fn login_row(at: i64) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"user","message":{{"role":"user","content":"<local-command-stdout>Login successful</local-command-stdout>"}},"uuid":"u","timestamp":"{}"}}"#,
        rfc3339_utc(at)
    )
}

/// An ordinary answer at `at`.
fn answer_row(at: i64) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","uuid":"a","timestamp":"{}","message":{{"id":"m","model":"claude-opus-5-5","role":"assistant","type":"message","usage":{{"input_tokens":2,"output_tokens":40}},"content":[{{"type":"text","text":"{SENTINEL} answer"}}]}}}}"#,
        rfc3339_utc(at)
    )
}

/// The home's `projects/` directory.
fn projects(env: &Env) -> PathBuf {
    env.claude_dir().expect("a Claude dir").join("projects")
}

/// A transcript `rel` under the home's `projects/` of `lines`, last written at `written`.
fn transcript(env: &Env, rel: &str, lines: &[String], written: i64) -> PathBuf {
    let path = projects(env).join(rel);
    std::fs::create_dir_all(path.parent().expect("its directory")).expect("mkdir");
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(&path, text).expect("write");
    let at = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(u64::try_from(written).expect("after 1970"));
    std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("open")
        .set_modified(at)
        .expect("set its last write");
    path
}

fn history(env: &Env, name: Option<&str>, (json, rows): (bool, bool)) -> String {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = crate::harness::cli::history::run(
        Cli::Claude,
        env,
        (name, json, SPAN, rows),
        utc,
        &mut out,
        &mut err,
    );
    let err = String::from_utf8_lossy(&err).into_owned();
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    String::from_utf8(out).expect("utf-8")
}

fn history_json(env: &Env, name: &str) -> Value {
    aterm_json::from_str(history(env, Some(name), (true, false)).trim()).expect("JSON")
}

/// The day lines of a text history.
fn day_lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|l| l.starts_with("  ") && !l.starts_with("    "))
        .collect()
}

/// Each backfilled close of a history's JSON as `(w, reset, n)`.
fn closes(doc: &Value) -> Vec<(String, i64, u64)> {
    doc["backfill"]["closes"]
        .as_array()
        .expect("closes")
        .iter()
        .map(|c| {
            assert_eq!(c["src"], Value::from("backfill"), "{c}");
            assert_eq!(c["walled"], Value::from(true), "{c}");
            assert_eq!(c["peak"], Value::from(1000u64), "{c}");
            (
                c["w"].as_str().expect("a window").to_owned(),
                c["reset"].as_i64().expect("a reset"),
                c["n"].as_u64().expect("walls"),
            )
        })
        .collect()
}

/// The series' first reading, three days before now: its five-hour window resets two hours
/// after it, its week two days after now.
fn series_start(env: &Env) -> (i64, i64, i64) {
    let start = env.now - 3 * D;
    (start, start + 2 * H, env.now + 2 * D)
}

/// `work`'s series from [`series_start`]: two readings of its first five-hour window and week.
fn seeded_series(env: &Env, machine: &str) -> PathBuf {
    let (start, five, week) = series_start(env);
    write_series(
        env,
        machine,
        &[
            reading(start, Some((200, five)), Some((300, week))),
            reading(start + H, Some((400, five)), Some((320, week))),
        ],
    )
}

/// A ONE-LOGIN HOME'S WALLS FROM BEFORE ITS SERIES ARE BOOKED, ONCE PER WINDOW. The series
/// begins on Thursday at 00:00. The transcripts hold two walls of one five-hour window on
/// Tuesday (resetting at 03:00), a weekly wall the week before (resetting on Saturday the 13th),
/// a wall at 23:00 on Wednesday of the five-hour window the series' first reading is in, and a
/// wall on Friday, after the series began, of a window it holds no reading of. History books the
/// Tuesday window once (its two walls) and the week, each `src: "backfill"`, full, on the day it
/// ended and marked `(backfill)`; the series' own days stay as they were. Negative control:
/// booking after the series' first reading (no `series_from` bound in `account_backfill::book`)
/// books Friday's wall as a close the series has covered, and the booked list fails.
#[test]
fn the_backfill_books_a_one_login_homes_walls_before_its_series() {
    let tmp = tmp_dir("backfill-books");
    let env = home_env(&tmp);
    let machine = roster(&tmp, &env, &[], None);
    seeded_series(&env, &machine);
    let (start, five, _) = series_start(&env);
    let tuesday = start - 2 * D + 3 * H;
    let week = start - 5 * D;
    transcript(
        &env,
        "-proj/older.jsonl",
        &[
            wall_row(week - 26 * H, accounts::WEEK_LIMIT, week),
            answer_row(week - 25 * H),
            wall_row(tuesday - 2 * H, accounts::SESSION_LIMIT, tuesday),
            wall_row(tuesday - H, accounts::SESSION_LIMIT, tuesday),
        ],
        tuesday - H,
    );
    transcript(
        &env,
        "-proj/newer.jsonl",
        &[
            wall_row(start - H, accounts::SESSION_LIMIT, five),
            wall_row(start + 30 * H, accounts::SESSION_LIMIT, start + 32 * H),
        ],
        start + 30 * H,
    );
    let doc = history_json(&env, "work");
    assert_eq!(
        closes(&doc),
        [
            (accounts::WEEK_LIMIT.to_owned(), week, 1),
            (accounts::SESSION_LIMIT.to_owned(), tuesday, 2),
        ],
        "{doc}"
    );
    assert_eq!(doc["backfill"]["booked"], Value::from(2u64));
    assert_eq!(doc["backfill"]["complete"], Value::from(true));
    assert_eq!(doc["backfill"]["cut"], Value::Null);
    assert_eq!(doc["backfill"]["why"], Value::Null);
    let text = history(&env, Some("work"), (false, false));
    assert_eq!(
        day_lines(&text),
        [
            "  Sat Sep 13  week 100% (resets Sat Sep 13 00:00) (backfill)",
            "  Tue Sep 16  5h 1 window, peak 100%, 1 full (backfill)",
            "  Thu Sep 18  week 30% \u{2192} 32% (resets Tue Sep 23 00:00) \u{b7} 5h 1 window, peak 40%",
        ],
        "{text}"
    );
    assert!(
        text.lines()
            .nth(1)
            .is_some_and(|header| !header.contains("in part")),
        "{text}"
    );
}

/// UNDER SEVERAL LOGINS NOTHING IS BOOKED. With `work` (the default home's login) and `play`
/// on the roster, the home's transcripts may hold either's walls: `work`'s history books none,
/// says why, and marks no day. Negative control: booking by the home whatever the roster holds
/// (no `several()` check in `Home::of`, and `another_seen` told of one login) books the wall to
/// `work`, and the first assertion fails.
#[test]
fn the_backfill_books_nothing_under_several_logins() {
    let tmp = tmp_dir("backfill-several");
    let env = home_env(&tmp);
    let machine = roster(&tmp, &env, &["play"], None);
    seeded_series(&env, &machine);
    let (start, _, _) = series_start(&env);
    let tuesday = start - 2 * D + 3 * H;
    transcript(
        &env,
        "-proj/s.jsonl",
        &[wall_row(tuesday - H, accounts::SESSION_LIMIT, tuesday)],
        tuesday - H,
    );
    let doc = history_json(&env, "work");
    assert_eq!(doc["backfill"]["booked"], Value::from(0u64), "{doc}");
    assert_eq!(doc["backfill"]["why"], Value::from("several-logins"));
    assert!(closes(&doc).is_empty());
    let play = history_json(&env, "play");
    assert_eq!(play["backfill"]["why"], Value::from("not-the-home"));
    let text = history(&env, None, (false, false));
    assert!(!text.contains("(backfill)"), "{text}");
}

/// NOTHING IS BOOKED FROM BEFORE ANOTHER ACCOUNT WAS SEEN ON THE HOME. `work`'s record was keyed
/// to another account on Saturday the 13th at 12:00 (`keyed_at`): of a wall on the 12th and one
/// on Monday the 15th, both before the series, only Monday's is booked. A `/login` that succeeded
/// on Tuesday the 16th at 12:00 stops it the same way: with it in a transcript, only the wall
/// on Wednesday the 17th after it is booked. Negative controls: ignoring `keyed_at` (no
/// `another_seen` in `account_backfill::book`'s bound) books the 12th's wall, and the first
/// assertion fails; ignoring the `/login` (no `unsure_through` in that bound) books Monday's,
/// and the second fails.
#[test]
fn the_backfill_stops_where_another_account_was_seen() {
    let tmp = tmp_dir("backfill-another");
    let env = home_env(&tmp);
    let (start, _, _) = series_start(&env);
    let keyed = start - 4 * D - 12 * H;
    let machine = roster(&tmp, &env, &[], Some(keyed));
    seeded_series(&env, &machine);
    let friday = start - 6 * D + 3 * H;
    let monday = start - 3 * D + 3 * H;
    let wednesday = start - D + 3 * H;
    transcript(
        &env,
        "-proj/a.jsonl",
        &[
            wall_row(friday - H, accounts::SESSION_LIMIT, friday),
            wall_row(monday - H, accounts::SESSION_LIMIT, monday),
            wall_row(wednesday - H, accounts::SESSION_LIMIT, wednesday),
        ],
        wednesday - H,
    );
    let resets = |doc: &Value| -> Vec<i64> { closes(doc).iter().map(|(_, r, _)| *r).collect() };
    assert_eq!(resets(&history_json(&env, "work")), [monday, wednesday]);
    let login = start - 2 * D + 12 * H;
    transcript(
        &env,
        "-proj/b.jsonl",
        &[answer_row(login - H), login_row(login)],
        login,
    );
    assert_eq!(resets(&history_json(&env, "work")), [wednesday]);
}

/// A READ IS CUT BY ITS BUDGET, AND SAYS SO. Two transcripts, the newer last written after the
/// older and holding a wall from before the older's last write: with a byte cap of the newer's
/// length, the read takes the newer's walls alone, says `bytes`, and books nothing the older
/// could have changed (nothing from before its last write); with a deadline already past, it
/// reads nothing and says `time`; with no bound it reads both, whole. Negative controls: a read
/// with no cap (`left` never checked in `account_backfill::read`) takes the older one's wall
/// too, and the first assertion fails; booking past the cut (no `unsure_through` in
/// `account_backfill::book`'s bound) books the early wall, and the booked count fails.
#[test]
fn the_backfill_is_cut_by_its_budget() {
    let tmp = tmp_dir("backfill-cut");
    let env = home_env(&tmp);
    let (start, _, _) = series_start(&env);
    let older_at = start - 3 * D + 3 * H;
    let newer_at = start - D + 3 * H;
    let early_at = start - 5 * D + 3 * H;
    transcript(
        &env,
        "-proj/older.jsonl",
        &[wall_row(older_at - H, accounts::SESSION_LIMIT, older_at)],
        older_at - H,
    );
    let newer = transcript(
        &env,
        "-proj/newer.jsonl",
        &[
            wall_row(early_at - H, accounts::SESSION_LIMIT, early_at),
            wall_row(newer_at - H, accounts::SESSION_LIMIT, newer_at),
        ],
        newer_at - H,
    );
    let cap = std::fs::metadata(&newer).expect("metadata").len();
    let bounded = Limits {
        bytes: cap,
        until: None,
    };
    let cut = account_backfill::read(&projects(&env), 0, &bounded, env.now);
    assert_eq!(
        cut.walls
            .iter()
            .map(|w| (w.at, w.reset))
            .collect::<Vec<_>>(),
        [(early_at - H, early_at), (newer_at - H, newer_at)],
        "{cut:?}"
    );
    assert_eq!(cut.cut, Some(Cut::Bytes));
    assert_eq!(cut.unsure_through, Some(older_at - H));
    let booked = account_backfill::book(&cut, &[], None, ((None, None), "m"), env.now);
    assert_eq!(booked.len(), 1, "{booked:?}");
    let Said::Close(close) = &booked[0].said else {
        panic!("{booked:?}")
    };
    assert_eq!(close.reset, newer_at, "{booked:?}");

    let past = Limits {
        bytes: u64::MAX,
        until: Some(std::time::Instant::now()),
    };
    let timed = account_backfill::read(&projects(&env), 0, &past, env.now);
    assert!(timed.walls.is_empty(), "{timed:?}");
    assert_eq!(timed.cut, Some(Cut::Time));
    assert!(account_backfill::book(&timed, &[], None, ((None, None), "m"), env.now).is_empty());

    let whole = account_backfill::read(&projects(&env), 0, &Limits::none(), env.now);
    assert_eq!(whole.walls.len(), 3, "{whole:?}");
    assert_eq!((whole.cut, whole.unsure_through), (None, None));
}

/// NO TRANSCRIPT TEXT IS KEPT. The wall rows carry the sentinel in their ids, working
/// directory, notice and credits word, and one names the sentinel as its window: the booked
/// close is there, and the sentinel is in no part of what the read holds, the history's text,
/// its JSON or its `--rows`. Negative control: keeping the notice text (the wall's message text
/// carried in the read's walls and into the close's `src`) puts it in what the read holds, and
/// the second assertion fails.
#[test]
fn the_backfill_keeps_no_transcript_text() {
    let tmp = tmp_dir("backfill-text");
    let env = home_env(&tmp);
    let machine = roster(&tmp, &env, &[], None);
    seeded_series(&env, &machine);
    let (start, _, _) = series_start(&env);
    let tuesday = start - 2 * D + 3 * H;
    transcript(
        &env,
        "-proj/s.jsonl",
        &[
            wall_row(tuesday - 2 * H, SENTINEL, tuesday),
            wall_row(tuesday - H, accounts::SESSION_LIMIT, tuesday),
            answer_row(tuesday),
        ],
        tuesday,
    );
    let read = account_backfill::read(&projects(&env), 0, &Limits::none(), env.now);
    assert_eq!(read.walls.len(), 1, "{read:?}");
    assert!(!format!("{read:?}").contains(SENTINEL), "{read:?}");
    let doc = history_json(&env, "work");
    assert_eq!(doc["backfill"]["booked"], Value::from(1u64), "{doc}");
    for printed in [
        history(&env, Some("work"), (false, false)),
        history(&env, Some("work"), (true, false)),
        history(&env, Some("work"), (false, true)),
        history(&env, Some("work"), (true, true)),
        history(&env, None, (true, true)),
    ] {
        assert!(
            !printed.contains(SENTINEL),
            "transcript text printed: {printed}"
        );
    }
}

/// Every entry under `dir` with its kind, length and last write.
fn listing(dir: &Path) -> Vec<(PathBuf, bool, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).expect("read dir").flatten() {
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).expect("metadata");
        out.push((
            path.clone(),
            meta.is_dir(),
            meta.len(),
            meta.modified().expect("mtime"),
        ));
        if meta.is_dir() {
            out.extend(listing(&path));
        }
    }
    out.sort();
    out
}

/// THE BACKFILL WRITES NOTHING. Over a series and transcripts whose walls it books, a named
/// history, one of every row, their JSON and `--rows` leave every file and directory (the
/// series, its directory, the transcripts, the home) as they were and add none, from the first
/// history on. Negative control: a history that appends its booked closes' lines to the
/// label's series file changes the series, and the last assertion fails.
#[test]
fn the_backfill_writes_nothing() {
    let tmp = tmp_dir("backfill-writes");
    let env = home_env(&tmp);
    let machine = roster(&tmp, &env, &[], None);
    let path = seeded_series(&env, &machine);
    let (start, _, _) = series_start(&env);
    let tuesday = start - 2 * D + 3 * H;
    transcript(
        &env,
        "-proj/s.jsonl",
        &[wall_row(tuesday - H, accounts::SESSION_LIMIT, tuesday)],
        tuesday - H,
    );
    transcript(
        &env,
        "-proj/s/subagents/agent-a.jsonl",
        &[wall_row(tuesday - 2 * H, accounts::SESSION_LIMIT, tuesday)],
        tuesday - 2 * H,
    );
    let series_dir = path.parent().expect("the series' directory").to_path_buf();
    let home = env.claude_dir().expect("the home");
    let kept = || {
        (
            std::fs::read(&path).expect("the series"),
            listing(&series_dir),
            listing(&home),
        )
    };
    let before = kept();
    assert_eq!(
        closes(&history_json(&env, "work")),
        [(accounts::SESSION_LIMIT.to_owned(), tuesday, 2)],
        "a subagent's wall counts in its window"
    );
    let was = listing(tmp.path());
    for name in [Some("work"), None] {
        for flags in [(false, false), (true, false), (false, true), (true, true)] {
            let _ = history(&env, name, flags);
        }
    }
    assert_eq!(listing(tmp.path()), was, "a history wrote");
    assert!(
        kept() == before,
        "a history wrote to the series or the home"
    );
}
