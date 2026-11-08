// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The account backfill in `aterm harness accounts history` ([`crate::harness::account_backfill`]):
//! a one-login home's transcript walls of windows its series holds no word of, booked in memory
//! as closes and marked `(backfill)`; nothing under several logins, or where nothing says whose
//! the home is; nothing from before another account was seen on the home, a `/login`, another
//! login's conversation or the series' own sight of the home changing hands; a read cut by its
//! budget or its walk, and saying so; no transcript text kept; nothing written. Every transcript
//! here is synthetic.

use super::*;
use crate::harness::account_backfill::{self, Cut};
use crate::harness::account_series::{self, Day, DayFive, DayWeek, Fig, Said, SeriesRow};
use crate::harness::profile::AccountKey;
use crate::harness::spend::Limits;

const H: i64 = 3600;
const D: i64 = 86_400;
const SPAN: i64 = 30 * D;
/// A word no booked row, printed line or JSON value may hold: the wall rows carry it in every
/// field a transcript row has text in.
const SENTINEL: &str = "sentinel_6e2b";
/// The account the default home's profile is signed in as, and its address.
const UUID: &str = "3c9a1e57-8b2d-4f60-a1c4-7e5d2b0f9a13";
const ADDRESS: &str = "work.sentinel_6e2b@example.test";
/// Another account, and its address.
const OTHER_UUID: &str = "b4e2d8c1-6a3f-4e95-8d07-1c2b3a4f5e6d";
const OTHER: &str = "other.sentinel_6e2b@example.test";

/// UTC: the zone the tests read local days in.
fn utc(_: i64) -> i64 {
    0
}

/// The default home's profile on `env`, signed in as `uuid` with `address` where one is given,
/// and holding a cached `/usage` body of another account fetched at `another_at` where that is.
fn sign_in(env: &Env, uuid: &str, address: Option<&str>, another_at: Option<i64>) {
    let mut oauth = aterm_json::json!({ "accountUuid": uuid });
    if let Some(address) = address {
        oauth
            .as_object_mut()
            .expect("an object")
            .insert("emailAddress".to_owned(), Value::from(address));
    }
    let mut profile = aterm_json::json!({ "oauthAccount": oauth });
    if let Some(at) = another_at {
        profile.as_object_mut().expect("an object").insert(
            "cachedUsageUtilization".to_owned(),
            aterm_json::json!({ "fetchedAtMs": at * 1000, "accountUuid": OTHER_UUID }),
        );
    }
    let path = env.claude_profile.clone().expect("a profile path");
    std::fs::create_dir_all(path.parent().expect("its directory")).expect("mkdir");
    std::fs::write(path, profile.to_string()).expect("write the profile");
}

/// `env_at`, naming the default Claude Code home (`~/.claude`, its profile `~/.claude.json`,
/// signed in as [`UUID`], [`ADDRESS`]).
fn home_env(tmp: &Tmp) -> Env {
    let mut env = env_at(tmp);
    env.claude_profile = env.home.as_ref().map(|home| home.join(".claude.json"));
    sign_in(&env, UUID, Some(ADDRESS), None);
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

/// `work`'s record keyed as the account `key`.
fn key_work(env: &Env, key: AccountKey) {
    let mut note = None;
    env.accounts_store()
        .transact(env.now, &mut note, |state| {
            state.accounts.entry("work".to_owned()).or_default().key = Some(key);
            Ok(Vec::new())
        })
        .expect("state");
}

fn reading(at: i64, five: Option<(u32, i64)>, week: Option<(u32, i64)>) -> SeriesRow {
    keyed(at, five, week, None)
}

fn keyed(
    at: i64,
    five: Option<(u32, i64)>,
    week: Option<(u32, i64)>,
    key: Option<AccountKey>,
) -> SeriesRow {
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
            key,
        },
    }
}

fn write_series(env: &Env, machine: &str, rows: &[SeriesRow]) -> PathBuf {
    let path = account_series::path_of(&env.state, "work", machine).expect("a series path");
    std::fs::create_dir_all(path.parent().expect("its directory")).expect("mkdir");
    let mut rows = rows.to_vec();
    rows.sort_by_key(|r| r.t);
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

/// A five-hour wall at `at` of the window that resets an hour later.
fn five_wall(at: i64) -> String {
    wall_row(at, accounts::SESSION_LIMIT, at + H)
}

/// A `/login` that succeeded, at `at`.
fn login_row(at: i64) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"user","message":{{"role":"user","content":"<local-command-stdout>Login successful</local-command-stdout>"}},"uuid":"u","timestamp":"{}"}}"#,
        rfc3339_utc(at)
    )
}

/// A conversation's `session_context` at `at` naming `address`, `changed` where the account
/// changed inside the conversation.
fn context_row(at: i64, address: &str, changed: bool) -> String {
    let change = if changed {
        r#","changed":true,"reason":"account_change""#
    } else {
        ""
    };
    format!(
        r#"{{"type":"attachment","timestamp":"{}","attachment":{{"type":"session_context","context":{{"userEmail":"The user's email address is {address}. Use it only to identify the user."}}{change}}}}}"#,
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

/// The resets of a history's backfilled closes.
fn resets(doc: &Value) -> Vec<i64> {
    closes(doc).iter().map(|(_, r, _)| *r).collect()
}

/// The series' first reading, three days before now (Thursday at 00:00): its five-hour window
/// resets two hours after it, its week two days after now.
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

/// A ONE-LOGIN HOME'S WALLS OF WINDOWS ITS SERIES HOLDS NO WORD OF ARE BOOKED, ONCE PER WINDOW.
/// The series reads Thursday from 00:00 to 01:00 and nothing after. The transcripts hold two
/// walls of one five-hour window on Tuesday (resetting at 03:00), a weekly wall the week before
/// (resetting on Saturday the 13th), a wall at 23:00 on Wednesday of the five-hour window the
/// series' first reading is in, and a wall on Friday, after the series began, of a window it did
/// not read (the tap off, or a session aterm did not host). History books the Tuesday window
/// once (its two walls), the week and Friday's window, each `src: "backfill"`, full, on the day
/// it ended and marked `(backfill)`; the series' own days stay as they were. Negative controls:
/// booking a window the series holds (no `held` check in `account_backfill::book`) books
/// Wednesday's wall as a second close of the series' first window, and booking only before the
/// series' first reading leaves Friday's window out; the booked list fails either way.
#[test]
fn the_backfill_books_a_one_login_homes_walls_the_series_holds_no_word_of() {
    let tmp = tmp_dir("backfill-books");
    let env = home_env(&tmp);
    let machine = roster(&tmp, &env, &[], None);
    seeded_series(&env, &machine);
    let (start, five, _) = series_start(&env);
    let tuesday = start - 2 * D + 3 * H;
    let week = start - 5 * D;
    let friday = start + 32 * H;
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
            wall_row(friday - 2 * H, accounts::SESSION_LIMIT, friday),
        ],
        friday - 2 * H,
    );
    let doc = history_json(&env, "work");
    assert_eq!(
        closes(&doc),
        [
            (accounts::WEEK_LIMIT.to_owned(), week, 1),
            (accounts::SESSION_LIMIT.to_owned(), tuesday, 2),
            (accounts::SESSION_LIMIT.to_owned(), friday, 1),
        ],
        "{doc}"
    );
    assert_eq!(doc["backfill"]["booked"], Value::from(3u64));
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
            "  Fri Sep 19  5h 1 window, peak 100%, 1 full (backfill)",
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

/// A DAY'S ROW WRITTEN BESIDE ITS READINGS MOVES NO WALL OUT OF THAT DAY. The series' first
/// reading is on Thursday at 18:00, and Thursday's `day` row, which compaction wrote on Friday as
/// it does beside the readings and the close it was made from, starts at Thursday's midnight. A
/// wall at 02:00 that Thursday, of a window that reset at 03:00, is still booked. Negative
/// control: taking a `day` row's midnight as the first instant the series read its day (the day
/// row counted in `held` with readings of its day beside it) leaves the 02:00 window out, and
/// the booked list fails.
#[test]
fn a_days_row_beside_its_readings_keeps_the_walls_before_them() {
    let tmp = tmp_dir("backfill-day-row");
    let env = home_env(&tmp);
    let machine = roster(&tmp, &env, &[], None);
    let (start, _, week) = series_start(&env);
    let five = start + 20 * H;
    let figs = vec![
        reading(start + 18 * H, Some((400, five)), Some((300, week))),
        reading(start + 19 * H, Some((600, five)), Some((320, week))),
    ];
    let mut rows = figs.clone();
    rows.extend(account_series::derive_closes(&figs, start + D + H));
    rows.extend(account_series::day_summaries(&rows, start, start + D, utc));
    assert!(
        rows.iter()
            .any(|r| r.t == start && matches!(r.said, Said::Day(_))),
        "Thursday's day row: {rows:?}"
    );
    write_series(&env, &machine, &rows);
    transcript(
        &env,
        "-proj/s.jsonl",
        &[five_wall(start + 2 * H)],
        start + 2 * H,
    );
    let doc = history_json(&env, "work");
    assert_eq!(
        closes(&doc),
        [(accounts::SESSION_LIMIT.to_owned(), start + 3 * H, 1)],
        "{doc}"
    );
}

/// A DAY THE SERIES HOLDS BY ITS ROW ALONE TAKES NO WINDOW OF IT FROM THE TRANSCRIPTS. Of
/// Thursday the series holds only its `day` row (three windows, one full), its readings and
/// closes gone as an old tier's are, and with them which windows they were. A wall at 23:00 on
/// Wednesday of a window that reset at 01:00 on Thursday may be one the row counts, so it is not
/// booked, and Thursday keeps its three windows; a wall on Tuesday is. Negative control: taking
/// the day's midnight as where the series began, with no window inside the day held (the old
/// `covered`), books Wednesday's wall as a close ending at Thursday's midnight, and Thursday
/// prints `5h 1 window, peak 100%, 1 full (backfill)`: the booked list fails.
#[test]
fn a_day_the_series_holds_by_its_row_alone_takes_no_window_of_it() {
    let tmp = tmp_dir("backfill-day-alone");
    let env = home_env(&tmp);
    let machine = roster(&tmp, &env, &[], None);
    let (start, _, week) = series_start(&env);
    let day = SeriesRow {
        t: start,
        by: "m".into(),
        said: Said::Day(Day {
            span_s: D,
            tz_s: 0,
            five_hour: Some(DayFive {
                closed: 3,
                peak: 1000,
                walled: 1,
                anchors: Vec::new(),
            }),
            seven_day: Some(DayWeek {
                first: 300,
                last: 320,
                reset: week,
                closed: false,
            }),
            n: 12,
            ac: None,
            key: None,
        }),
    };
    write_series(&env, &machine, &[day]);
    let tuesday = start - 2 * D + 2 * H;
    transcript(
        &env,
        "-proj/s.jsonl",
        &[
            five_wall(tuesday),
            wall_row(start - H, accounts::SESSION_LIMIT, start + H),
        ],
        start - H,
    );
    let doc = history_json(&env, "work");
    assert_eq!(
        closes(&doc),
        [(accounts::SESSION_LIMIT.to_owned(), tuesday + H, 1)],
        "{doc}"
    );
    let text = history(&env, Some("work"), (false, false));
    assert_eq!(
        day_lines(&text),
        [
            "  Tue Sep 16  5h 1 window, peak 100%, 1 full (backfill)",
            "  Thu Sep 18  week 30% \u{2192} 32% (resets Tue Sep 23 00:00) \u{b7} 5h 3 windows, peak 100%, 1 full",
        ],
        "{text}"
    );
}

/// A WINDOW A DAY ROW NAMES IS HELD BESIDE ANOTHER FILE'S READINGS OF ITS DAY. The row `before`
/// holds only Thursday's `day` row, which names the window that reset at 06:00 by its anchor;
/// the renamed row's file still holds Thursday's readings and close of the window that reset at
/// 12:00. A transcript wall inside the 06:00 window is not booked, since the series names it;
/// CONTROL: with `before` holding nothing, the same wall is. Negative control: `held` reading a
/// `day` row's five-hour windows only as its whole day (no anchor of it taken), which the other
/// file's readings of Thursday set aside, books the wall, and the first assertion fails.
#[test]
fn a_window_a_day_row_names_is_held_beside_another_files_readings_of_its_day() {
    let tmp = tmp_dir("backfill-day-anchor");
    let env = home_env(&tmp);
    let (start, _, _) = series_start(&env);
    let (named, read) = (start + 6 * H, start + 12 * H);
    let before = vec![SeriesRow {
        t: start,
        by: "m".into(),
        said: Said::Day(Day {
            span_s: D,
            tz_s: 0,
            five_hour: Some(DayFive {
                closed: 1,
                peak: 600,
                walled: 0,
                anchors: vec![(named, false)],
            }),
            seven_day: None,
            n: 2,
            ac: None,
            key: None,
        }),
    }];
    let figs = vec![
        reading(start + 8 * H, Some((300, read)), None),
        reading(start + 10 * H, Some((500, read)), None),
    ];
    let mut renamed = figs.clone();
    renamed.extend(account_series::derive_closes(&figs, env.now));
    let walls = account_backfill::Walls {
        walls: vec![account_backfill::Wall {
            at: named - 2 * H,
            reset: named,
            w: accounts::SESSION_LIMIT,
        }],
        ..account_backfill::Walls::default()
    };
    let book = |files: &[Vec<SeriesRow>]| {
        account_backfill::book(&walls, files, None, ((None, None), "m"), env.now)
    };
    let booked = book(&[before, renamed.clone()]);
    assert!(booked.is_empty(), "{booked:?}");
    assert_eq!(
        book(&[Vec::new(), renamed])
            .iter()
            .map(|r| r.t)
            .collect::<Vec<_>>(),
        [named],
        "CONTROL: a window the series does not name is booked"
    );
}

/// UNDER SEVERAL LOGINS NOTHING IS READ OR BOOKED. With `work` (the default home's login) and
/// `play` on the roster, the home's transcripts may hold either's walls: `work`'s history books
/// none, says why and that nothing was read (`complete: null`), and marks no day. Negative
/// controls: booking by the home whatever the roster holds (no `several()` check in
/// `Home::look`) books the wall to `work`, and the first assertion fails; `complete` said as
/// `cut.is_none()` says `true` of the read that never ran, and the third fails.
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
    assert_eq!(doc["backfill"]["complete"], Value::Null, "{doc}");
    assert!(closes(&doc).is_empty());
    let play = history_json(&env, "play");
    assert_eq!(play["backfill"]["why"], Value::from("not-the-home"));
    assert_eq!(play["backfill"]["complete"], Value::Null, "{play}");
    let text = history(&env, None, (false, false));
    assert!(!text.contains("(backfill)"), "{text}");
}

/// NOTHING IS BOOKED FROM BEFORE ANOTHER ACCOUNT WAS SEEN ON THE HOME. `work`'s record was keyed
/// to another account on Saturday the 13th at 12:00 (`keyed_at`): of a wall on the 12th and one
/// on Monday the 15th, both before the series, only Monday's is booked. A `/login` that succeeded
/// on Tuesday the 16th at 12:00 stops it the same way: with it in a transcript, only the wall on
/// Wednesday the 17th after it is booked. So does the profile's cached `/usage` body of another
/// account, fetched on Wednesday at 12:00: then none is. Negative controls: ignoring `keyed_at`
/// (no `another_seen` in `account_backfill::book`'s bound) books the 12th's wall, and the first
/// assertion fails; ignoring the `/login` (no `unsure_through` in that bound) books Monday's, and
/// the second fails; leaving `usage_of_another_at` out of `account_stats::another_seen` books
/// Wednesday's, and the third fails.
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
    assert_eq!(resets(&history_json(&env, "work")), [monday, wednesday]);
    let login = start - 2 * D + 12 * H;
    transcript(
        &env,
        "-proj/b.jsonl",
        &[answer_row(login - H), login_row(login)],
        login,
    );
    assert_eq!(resets(&history_json(&env, "work")), [wednesday]);
    sign_in(&env, UUID, Some(ADDRESS), Some(start - D + 12 * H));
    assert_eq!(resets(&history_json(&env, "work")), Vec::<i64>::new());
}

/// NOTHING IS BOOKED FROM BEFORE THE SERIES SAW THE HOME CHANGE HANDS. `work` held account A,
/// whose readings it recorded on Friday the 12th and Sunday the 14th; A's record was then
/// forgotten and the row added again for B, which the home is signed in to, with no `/login` in
/// any transcript and no key change on the new record (it had none), so no `keyed_at` and no
/// event row: only A's keys on A's rows tell. B's readings begin on Thursday the 18th. A wall on
/// Saturday the 13th, between A's readings, and one on Tuesday the 16th, before B's first
/// reading, may be A's and are not booked; a wall on Friday the 19th, after B's first reading,
/// of a window the series did not read, is. Negative control: taking no bound from what the
/// series shows (no `taken_over` in the bound `book` is given) books the two earlier walls, and
/// the booked list fails.
#[test]
fn the_backfill_stops_where_the_series_saw_the_home_change_hands() {
    let tmp = tmp_dir("backfill-handed");
    let env = home_env(&tmp);
    let machine = roster(&tmp, &env, &[], None);
    let a = AccountKey::claude(OTHER_UUID).expect("a key");
    let b = AccountKey::claude(UUID).expect("a key");
    key_work(&env, b);
    let (start, five, week) = series_start(&env);
    let week_a = start - 4 * D;
    write_series(
        &env,
        &machine,
        &[
            keyed(start - 6 * D, None, Some((100, week_a)), Some(a)),
            keyed(start - 4 * D - H, None, Some((300, week_a)), Some(a)),
            keyed(start, Some((200, five)), Some((300, week)), Some(b)),
            keyed(start + H, Some((400, five)), Some((320, week)), Some(b)),
        ],
    );
    let (saturday, tuesday, friday) =
        (start - 5 * D + 3 * H, start - 2 * D + 2 * H, start + 30 * H);
    transcript(
        &env,
        "-proj/s.jsonl",
        &[five_wall(saturday), five_wall(tuesday), five_wall(friday)],
        friday,
    );
    assert_eq!(resets(&history_json(&env, "work")), [friday + H]);
}

/// NOTHING IS BOOKED FROM BEFORE ANOTHER LOGIN'S CONVERSATION ENDED. The profile names
/// [`ADDRESS`]. A conversation on Monday named another address in its `session_context` at
/// 08:00 and the home's at 12:00 (the account changed inside it, with no `/login` row: a sign-in
/// at first run writes none): of its walls at 09:00 and 13:00 only the second is booked. A
/// conversation on Tuesday that named the other address from 10:00 to its end at 11:00 then
/// stops every wall before 11:00, and one at 15:00 of a conversation of the home's address is
/// booked alone. Neither address is printed. Negative control: a fold that reads no
/// `session_context` books Monday's 09:00 wall, and the first assertion fails.
#[test]
fn the_backfill_stops_where_another_login_held_a_conversation() {
    let tmp = tmp_dir("backfill-context");
    let env = home_env(&tmp);
    let machine = roster(&tmp, &env, &[], None);
    seeded_series(&env, &machine);
    let (start, _, _) = series_start(&env);
    let monday = start - 3 * D;
    let tuesday = start - 2 * D;
    transcript(
        &env,
        "-proj/monday.jsonl",
        &[
            context_row(monday + 8 * H, OTHER, false),
            five_wall(monday + 9 * H),
            context_row(monday + 12 * H, ADDRESS, true),
            five_wall(monday + 13 * H),
        ],
        monday + 13 * H,
    );
    assert_eq!(resets(&history_json(&env, "work")), [monday + 14 * H]);
    transcript(
        &env,
        "-proj/tuesday-other.jsonl",
        &[
            context_row(tuesday + 10 * H, OTHER, false),
            answer_row(tuesday + 11 * H),
        ],
        tuesday + 11 * H,
    );
    transcript(
        &env,
        "-proj/tuesday.jsonl",
        &[
            context_row(tuesday + 14 * H, ADDRESS, false),
            five_wall(tuesday + 15 * H),
        ],
        tuesday + 15 * H,
    );
    assert_eq!(resets(&history_json(&env, "work")), [tuesday + 16 * H]);
    for printed in [
        history(&env, Some("work"), (false, true)),
        history(&env, None, (true, true)),
    ] {
        assert!(
            !printed.contains(OTHER) && !printed.contains(ADDRESS),
            "{printed}"
        );
    }
}

/// NOTHING IS READ OR BOOKED WHERE NOTHING SAYS WHOSE THE HOME IS. Over a wall the home's
/// transcripts hold, `work`'s history books none and says why, with `complete: null`: with no
/// profile (`no-profile`), with one that names an account but no address (`no-profile`: no
/// conversation's address can be compared with it), with a record of another account than the
/// profile's (`another-account`), and with no Claude Code directory (`no-home`). Negative
/// control: booking where the profile could not be read (no `no-profile` refusal in
/// `Home::look`, the key unknown and nothing gated) books the wall, and the first assertion
/// fails.
#[test]
fn the_backfill_reads_nothing_it_cannot_attribute() {
    let tmp = tmp_dir("backfill-unknown");
    let env = home_env(&tmp);
    let machine = roster(&tmp, &env, &[], None);
    seeded_series(&env, &machine);
    let (start, _, _) = series_start(&env);
    transcript(&env, "-proj/s.jsonl", &[five_wall(start - D)], start - D);
    assert_eq!(resets(&history_json(&env, "work")), [start - D + H]);
    let refused = |env: &Env, why: &str| {
        let doc = history_json(env, "work");
        assert_eq!(doc["backfill"]["why"], Value::from(why), "{doc}");
        assert_eq!(doc["backfill"]["booked"], Value::from(0u64), "{doc}");
        assert_eq!(doc["backfill"]["complete"], Value::Null, "{doc}");
        assert!(closes(&doc).is_empty(), "{doc}");
    };
    let profile = env.claude_profile.clone().expect("a profile path");
    std::fs::remove_file(&profile).expect("remove the profile");
    refused(&env, "no-profile");
    sign_in(&env, UUID, None, None);
    refused(&env, "no-profile");
    sign_in(&env, UUID, Some(ADDRESS), None);
    key_work(&env, AccountKey::claude(OTHER_UUID).expect("a key"));
    refused(&env, "another-account");
    let mut homeless = env.clone();
    homeless.claude_config_unusable = Some("relative/claude".into());
    refused(&homeless, "no-home");
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
    let cut = account_backfill::read(&projects(&env), ADDRESS, 0, &bounded, env.now);
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
    let timed = account_backfill::read(&projects(&env), ADDRESS, 0, &past, env.now);
    assert!(timed.walls.is_empty(), "{timed:?}");
    assert_eq!(timed.cut, Some(Cut::Time));
    assert!(account_backfill::book(&timed, &[], None, ((None, None), "m"), env.now).is_empty());

    let whole = account_backfill::read(&projects(&env), ADDRESS, 0, &Limits::none(), env.now);
    assert_eq!(whole.walls.len(), 3, "{whole:?}");
    assert_eq!((whole.cut, whole.unsure_through), (None, None));
}

/// A WALK CUT AT ITS BOUNDS BOOKS NOTHING, AND THE HISTORY SAYS SO. Beside a transcript holding a
/// wall, the home's `projects/` holds directories nested deeper than the walk goes: the read
/// says `walk`, `complete: false`, books nothing (a transcript it did not list may hold a row of
/// any time), and the text's header says `transcripts read in part (walk)`. Negative controls:
/// a header that never names the cut fails the last assertion; a read that takes the walk's
/// cut for a whole one books the wall, and the first fails.
#[test]
fn a_walk_cut_at_its_bounds_books_nothing_and_says_so() {
    let tmp = tmp_dir("backfill-walk");
    let env = home_env(&tmp);
    let machine = roster(&tmp, &env, &[], None);
    seeded_series(&env, &machine);
    let (start, _, _) = series_start(&env);
    transcript(&env, "-proj/s.jsonl", &[five_wall(start - D)], start - D);
    let deep = (0..crate::harness::spend::MAX_DEPTH)
        .fold(projects(&env), |dir, depth| dir.join(format!("d{depth}")));
    std::fs::create_dir_all(&deep).expect("mkdir");
    let doc = history_json(&env, "work");
    assert!(closes(&doc).is_empty(), "{doc}");
    assert_eq!(doc["backfill"]["cut"], Value::from("walk"), "{doc}");
    assert_eq!(doc["backfill"]["complete"], Value::from(false), "{doc}");
    let text = history(&env, Some("work"), (false, false));
    assert!(
        text.lines()
            .nth(1)
            .is_some_and(|header| header.ends_with("transcripts read in part (walk)")),
        "{text}"
    );
}

/// NO TRANSCRIPT TEXT IS KEPT. The wall rows carry the sentinel in their ids, working
/// directory, notice and credits word, one names the sentinel as its window, and the
/// conversation's `session_context` carries it in the home's address: the booked close is
/// there, and the sentinel is in no part of what the read holds, the history's text, its JSON or
/// its `--rows`. Negative control: keeping the notice text (the wall's message text carried in
/// the read's walls and into the close's `src`) puts it in what the read holds, and the second
/// assertion fails.
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
            context_row(tuesday - 3 * H, ADDRESS, false),
            wall_row(tuesday - 2 * H, SENTINEL, tuesday),
            wall_row(tuesday - H, accounts::SESSION_LIMIT, tuesday),
            answer_row(tuesday),
        ],
        tuesday,
    );
    let read = account_backfill::read(&projects(&env), ADDRESS, 0, &Limits::none(), env.now);
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
