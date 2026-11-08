// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm harness accounts history` ([`crate::harness::cli::history::run`]): one line a local
//! day from this Mac's readings series with that day's ledger lines beneath, every roster row in
//! roster order, a renamed row's old series found by its key, JSON that only adds keys, and
//! nothing written.

use super::*;
use crate::harness::account_series::{self, Fig, Said, SeriesRow};
use crate::harness::profile::AccountKey;

const H: i64 = 3600;
const D: i64 = 86_400;
const SPAN: i64 = 30 * D;
const UUID: &str = "5d0c1f2e-3a4b-4c5d-8e9f-0a1b2c3d4e5f";
const OTHER_UUID: &str = "9e8d7c6b-5a49-4382-b716-0f1e2d3c4b5a";
const SENTINEL: &str = "SENTINEL-WHY-6f1c";

/// Seven hours west of UTC: the zone the tests read local days in.
fn west(_: i64) -> i64 {
    -7 * H
}

/// A pool on `env` of `labels`, in that order, each with a login and a record (`keys` gives a
/// label's account key); the state's machine id.
fn pool(tmp: &Tmp, env: &Env, labels: &[&str], keys: &[(&str, AccountKey)]) -> String {
    let mut text = String::new();
    for label in labels {
        let dir = tmp.path().join("logins").join(label);
        login_made_in(&dir);
        text = accounts::append_row(&text, label, Class::Preferred, dir.to_str())
            .expect("the row admits");
    }
    std::fs::write(env.accounts_roster().expect("a config dir"), text).expect("write");
    let mut note = None;
    env.accounts_store()
        .transact(env.now, &mut note, |state| {
            for label in labels {
                state.accounts.entry((*label).to_owned()).or_default();
            }
            for (label, key) in keys {
                state.accounts.entry((*label).to_owned()).or_default().key = Some(*key);
            }
            Ok(Vec::new())
        })
        .expect("state");
    env.accounts_store().load(env.now).expect("load").0.machine
}

/// A ledger row about `label` at `t` whose free text is the sentinel.
fn ledger_row(env: &Env, label: &str, t: i64) {
    let mut event = accounts::Event::new(t, accounts::Kind::Auth, label);
    event.why = SENTINEL.into();
    let mut note = None;
    env.accounts_store()
        .transact(env.now, &mut note, |_| Ok(vec![event]))
        .expect("ledger");
}

fn fig(pct_x10: u32, reset: i64, at: i64) -> Fig {
    Fig {
        pct_x10,
        reset,
        at,
        src: accounts::SOURCE_TAP.into(),
        epoch: None,
    }
}

fn reading(
    at: i64,
    five: Option<(u32, i64)>,
    week: Option<(u32, i64)>,
    key: Option<AccountKey>,
) -> SeriesRow {
    SeriesRow {
        t: at,
        by: "m".into(),
        said: Said::Fig {
            five_hour: five.map(|(p, r)| fig(p, r, at)),
            seven_day: week.map(|(p, r)| fig(p, r, at)),
            ac: None,
            key,
        },
    }
}

fn write_series(env: &Env, label: &str, machine: &str, rows: &[SeriesRow]) -> PathBuf {
    let path = account_series::path_of(&env.state, label, machine).expect("a series path");
    std::fs::create_dir_all(path.parent().expect("its directory")).expect("mkdir");
    let text: String = rows
        .iter()
        .map(|r| r.line().expect("a row") + "\n")
        .collect();
    std::fs::write(&path, text).expect("write");
    path
}

fn history_of(
    env: &Env,
    name: Option<&str>,
    (json, rows): (bool, bool),
    tz_at: fn(i64) -> i64,
) -> String {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = crate::harness::cli::history::run(
        Cli::Claude,
        env,
        (name, json, SPAN, rows),
        tz_at,
        &mut out,
        &mut err,
    );
    let err = String::from_utf8_lossy(&err).into_owned();
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    String::from_utf8(out).expect("utf-8")
}

/// The day lines of a text history (two-space indent), with the lines indented under each.
fn day_lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|l| l.starts_with("  ") && !l.starts_with("    "))
        .collect()
}

/// `work`'s seed: a reading at 19:00 local on Friday and one at 09:00 on Saturday (both
/// Saturday in UTC), each in its own five-hour window, and a ledger row at 20:00 on Friday
/// whose free text is the sentinel.
fn seeded_work(tmp: &Tmp, env: &Env) -> (String, PathBuf) {
    let now = env.now;
    let machine = pool(tmp, env, &["work"], &[]);
    let week = now + 3 * D;
    let path = write_series(
        env,
        "work",
        &machine,
        &[
            reading(
                now - 22 * H,
                Some((300, now - 20 * H)),
                Some((400, week)),
                None,
            ),
            reading(
                now - 8 * H,
                Some((600, now - 4 * H)),
                Some((470, week)),
                None,
            ),
        ],
    );
    ledger_row(env, "work", now - 21 * H);
    (machine, path)
}

/// A HISTORY PRINTS ONE LINE A LOCAL DAY, WITH ITS LEDGER LINES UNDER IT. Seven hours west of
/// UTC, a reading at 19:00 on Friday and one at 09:00 on Saturday (both Saturday in UTC) are two
/// days, oldest first, each with its week, its reset on the local clock and its closed
/// five-hour window; the ledger row at 20:00 on Friday is printed under Friday as the ledger
/// line it is, and its free text never is, in text or JSON. Negative control: reading the days
/// in UTC (`history_of(.., |_| 0)`) gives one day, Saturday, and the first assertion fails.
#[test]
fn history_prints_a_line_per_local_day_with_its_ledger_lines() {
    let tmp = tmp_dir("history-days");
    let env = env_at(&tmp);
    seeded_work(&tmp, &env);
    let text = history_of(&env, Some("work"), (false, false), west);
    assert_eq!(
        day_lines(&text),
        [
            "  Fri Sep 19  week 40% (resets Tue Sep 23 17:00) \u{b7} 5h 1 window, peak 30%",
            "  Sat Sep 20  week 47% (resets Tue Sep 23 17:00) \u{b7} 5h 1 window, peak 60%",
        ],
        "{text}"
    );
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[0],
        "(this Mac's ledger only: no lab journal is read in this process)"
    );
    assert!(
        lines[1].starts_with("work \u{b7} claude \u{b7} this Mac \u{b7} ")
            && lines[1].ends_with(" (30d) \u{b7} series since Fri Sep 19"),
        "{text}"
    );
    let friday = lines
        .iter()
        .position(|l| l.starts_with("  Fri Sep 19"))
        .expect("Friday");
    assert_eq!(
        lines[friday + 1],
        "    2025-09-20T03:00:00Z here auth work",
        "the ledger line under its local day: {text}"
    );
    assert!(lines[friday + 2].starts_with("  Sat Sep 20"), "{text}");
    let json = history_of(&env, Some("work"), (true, false), west);
    for printed in [&text, &json] {
        assert!(!printed.contains(SENTINEL), "free text printed: {printed}");
    }
    let doc: Value = aterm_json::from_str(json.trim()).expect("JSON");
    let days = doc["days"].as_array().expect("days");
    assert_eq!(
        days.iter()
            .map(|d| d["date"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["2025-09-19", "2025-09-20"]
    );
    assert_eq!(days[0]["tz_s"], Value::from(-7 * H));
    assert_eq!(days[0]["five_hour"]["windows"], Value::from(1u64));
    assert_eq!(days[0]["five_hour"]["peak_pct_x10"], Value::from(300u64));
    assert_eq!(days[1]["seven_day"]["last_pct_x10"], Value::from(470u64));
    assert_eq!(
        days[1]["seven_day"]["resets_at"],
        Value::from(env.now + 3 * D)
    );
}

/// A HISTORY BY KEY FINDS A RENAMED ROW'S SERIES. The row `now` is the account whose key its
/// record holds; a series kept under its old label `before` carries that key and is read with
/// it, whether the account is named by its label or by its key (`c-…`), and a third series of
/// another account is not. Negative control: reading the label's own series alone (no file
/// found by key in `by_key`) loses the old label's day, `Fri Sep 19`, and the first assertion
/// fails.
#[test]
fn history_by_key_finds_a_renamed_rows_series() {
    let tmp = tmp_dir("history-key");
    let env = env_at(&tmp);
    let now = env.now;
    let key = AccountKey::claude(UUID).expect("key");
    let other = AccountKey::claude(OTHER_UUID).expect("key");
    let machine = pool(&tmp, &env, &["now"], &[("now", key)]);
    let week = now + 3 * D;
    write_series(
        &env,
        "before",
        &machine,
        &[reading(now - 2 * D, None, Some((200, week)), Some(key))],
    );
    write_series(
        &env,
        "now",
        &machine,
        &[reading(now - 2 * H, None, Some((300, week)), None)],
    );
    write_series(
        &env,
        "stranger",
        &machine,
        &[reading(now - 3 * D, None, Some((900, week)), Some(other))],
    );
    let by_label = history_of(&env, Some("now"), (false, false), |_| 0);
    assert_eq!(
        day_lines(&by_label)
            .iter()
            .map(|l| l.get(..12).unwrap_or(l))
            .collect::<Vec<_>>(),
        ["  Fri Sep 19", "  Sat Sep 20"],
        "the old label's day and the new one's, none of the stranger's: {by_label}"
    );
    assert!(by_label.contains("week 20% "), "{by_label}");
    assert!(!by_label.contains("week 90% "), "{by_label}");
    assert!(
        by_label.contains(" \u{b7} series since Fri Sep 19\n"),
        "{by_label}"
    );
    let by_key = history_of(&env, Some(&key.to_string()), (false, false), |_| 0);
    assert_eq!(by_key, by_label, "the key names the same account");
    assert!(
        by_key
            .lines()
            .nth(1)
            .is_some_and(|l| l.starts_with("now \u{b7} claude"))
    );
}

/// A HISTORY WITHOUT A NAME LISTS EVERY ROW, IN ROSTER ORDER. Three rows listed `zeta`, `alpha`,
/// `mid` print in that order in text and JSON, never sorted, and the row with nothing recorded
/// says so. Negative control: listing the roster sorted by label puts `alpha` first and the
/// first assertion fails.
#[test]
fn history_without_a_name_lists_every_row_in_roster_order() {
    let tmp = tmp_dir("history-all");
    let env = env_at(&tmp);
    let now = env.now;
    let machine = pool(&tmp, &env, &["zeta", "alpha", "mid"], &[]);
    let week = now + 3 * D;
    write_series(
        &env,
        "zeta",
        &machine,
        &[reading(now - 2 * H, None, Some((300, week)), None)],
    );
    write_series(
        &env,
        "mid",
        &machine,
        &[reading(now - 5 * H, None, Some((800, week)), None)],
    );
    let text = history_of(&env, None, (false, false), |_| 0);
    let heads: Vec<&str> = text
        .lines()
        .filter(|l| l.contains(" \u{b7} claude \u{b7} this Mac \u{b7} "))
        .map(|l| l.split(' ').next().unwrap_or_default())
        .collect();
    assert_eq!(heads, ["zeta", "alpha", "mid"], "{text}");
    let alpha = text
        .split("\n\n")
        .find(|block| block.contains("alpha \u{b7} claude"))
        .expect("alpha's block");
    assert!(alpha.contains("\n  nothing recorded in 30d"), "{alpha}");
    let doc: Value =
        aterm_json::from_str(history_of(&env, None, (true, false), |_| 0).trim()).expect("JSON");
    assert_eq!(doc["kind"], Value::from("account-history"));
    assert_eq!(doc["schema"], Value::from(1u64));
    let labels: Vec<&str> = doc["accounts"]
        .as_array()
        .expect("accounts")
        .iter()
        .map(|a| a["label"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(labels, ["zeta", "alpha", "mid"]);
    assert_eq!(doc["accounts"][1]["days"], Value::Array(Vec::new()));
}

/// A HISTORY'S JSON ONLY ADDS KEYS. The lab model's `{label, ac, journal, lines}` keep their
/// meaning, `lines` every ledger and journal line as before, one older than the span included;
/// `kind`, `schema`, `span`, `series` and `days` are added, and `rows` only with `--rows`, each
/// stored line inside the span exactly as stored. Another CLI's pool says `series: false`.
/// Negative control: `lines` cut to the span (the `Block::json` lines filtered by
/// `span.holds`) drops the older line, and the `lines` assertion fails.
#[test]
fn history_json_only_adds_keys() {
    let tmp = tmp_dir("history-json");
    let env = env_at(&tmp);
    let (_, path) = seeded_work(&tmp, &env);
    ledger_row(&env, "work", env.now - 40 * D);
    let doc: Value =
        aterm_json::from_str(history_of(&env, Some("work"), (true, false), west).trim())
            .expect("JSON");
    let keys: Vec<&str> = doc
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    let mut want = vec![
        "ac", "backfill", "days", "journal", "kind", "label", "lines", "schema", "series", "span",
    ];
    want.sort_unstable();
    assert_eq!(keys, want);
    assert_eq!(doc["label"], Value::from("work"));
    assert_eq!(doc["ac"], Value::Null);
    assert_eq!(doc["journal"], Value::from(false));
    assert_eq!(
        doc["lines"],
        Value::Array(vec![
            Value::from("2025-08-12T00:00:00Z here auth work"),
            Value::from("2025-09-20T03:00:00Z here auth work"),
        ]),
        "every line, as the lab model's history gave them"
    );
    assert_eq!(doc["span"]["from"], Value::from(env.now - SPAN));
    assert_eq!(doc["span"]["to"], Value::from(env.now));
    assert_eq!(doc["series"]["oldest"], Value::from(env.now - 22 * H));
    assert!(doc["series"]["machine"].as_str().is_some());
    let with_rows: Value =
        aterm_json::from_str(history_of(&env, Some("work"), (true, true), west).trim())
            .expect("JSON");
    let stored: Vec<Value> = std::fs::read_to_string(&path)
        .expect("the series")
        .lines()
        .map(Value::from)
        .collect();
    assert_eq!(with_rows["rows"], Value::Array(stored));
    let text = history_of(&env, Some("work"), (false, true), west);
    for line in std::fs::read_to_string(&path).expect("the series").lines() {
        assert!(text.lines().any(|l| l == line), "{line} in {text}");
    }

    let codex_roster = env.accounts_roster_for(Cli::Codex).expect("a codex roster");
    let codex = accounts::Row {
        label: "cx".into(),
        ..Default::default()
    };
    std::fs::write(
        &codex_roster,
        accounts::append_rows_for(Cli::Codex, "", &[codex]).expect("the row admits"),
    )
    .expect("write");
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = crate::harness::cli::history::run(
        Cli::Codex,
        &env,
        (Some("cx"), true, SPAN, false),
        west,
        &mut out,
        &mut err,
    );
    assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&err));
    let doc: Value = aterm_json::from_slice(&out).expect("JSON");
    assert_eq!(doc["series"], Value::from(false));
    assert_eq!(doc["label"], Value::from("cx"));
    assert!(doc["lines"].is_array() && doc.get("days").is_none());
}

/// Every entry under `dir` with its kind, length and modification time.
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

/// A HISTORY WRITES NOTHING. Over a series a compaction would rewrite (a five-hour window that
/// ended with no close stored, and no pass stamped), a named history, one of every row, their
/// JSON and `--rows` leave every file and directory as they were, its length and mtime included,
/// and add none. Negative control: a history that compacts what it reads
/// (`account_series::compact_dir` on the series directory in `run`) rewrites the series and
/// writes the stamp, and the assertion fails.
#[test]
fn history_writes_nothing() {
    let tmp = tmp_dir("history-writes");
    let env = env_at(&tmp);
    let (_, path) = seeded_work(&tmp, &env);
    let dir = account_series::dir_in(&env.state);
    assert!(
        !dir.join(account_series::COMPACTED_FILE).exists(),
        "no pass was stamped"
    );
    let before = std::fs::read(&path).expect("the series");
    assert!(
        account_series::derive_closes(&account_series::read_file(&path), env.now)
            .iter()
            .any(|r| matches!(r.said, Said::Close(_))),
        "a compaction would store closes"
    );
    let was = listing(tmp.path());
    for name in [Some("work"), None] {
        for flags in [(false, false), (true, false), (false, true), (true, true)] {
            let _ = history_of(&env, name, flags, west);
        }
    }
    assert_eq!(listing(tmp.path()), was, "a history wrote");
    assert_eq!(std::fs::read(&path).expect("the series"), before);
}

fn keyed(
    at: i64,
    five: Option<(u32, i64)>,
    week: Option<(u32, i64)>,
    key: AccountKey,
) -> SeriesRow {
    reading(at, five, week, Some(key))
}

fn event(t: i64) -> SeriesRow {
    SeriesRow {
        t,
        by: "m".into(),
        said: Said::Event(account_series::EventSaid::of(
            account_series::EventWord::AnotherAccount,
        )),
    }
}

/// The day lines of `text` cut to their dates.
fn dates(text: &str) -> Vec<&str> {
    day_lines(text)
        .iter()
        .map(|l| l.get(..12).unwrap_or(l))
        .collect()
}

/// A HISTORY HOLDS ONLY THE ACCOUNT'S OWN READINGS. Account A's readings sit under `work`, the
/// label it held until another account, B, signed in to its home on Friday at 10:00 (the event
/// row), both five-hour windows ending at 12:00; A was then the row `now`, and B also used A's
/// old label `before` on Saturday. `history now` (A) holds A's days from every file that carries
/// its key, with the sign-in under Friday, and none of B's readings; `history work` (B) holds
/// B's alone, without the sign-in it made, and no reset used where A's 80% gave way to B's 20%
/// under one reset. Negative control: keeping every line of a file that names the account
/// (`judged` returning the lines as read) puts B's week of 10% in A's Friday, and the first
/// assertion fails.
#[test]
fn history_holds_only_the_accounts_own_readings() {
    let tmp = tmp_dir("history-own");
    let env = env_at(&tmp);
    let now = env.now;
    let a = AccountKey::claude(UUID).expect("key");
    let b = AccountKey::claude(OTHER_UUID).expect("key");
    let machine = pool(&tmp, &env, &["now", "work"], &[("now", a), ("work", b)]);
    let (week, week_b, five) = (now + 3 * D, now + 5 * D, now - 36 * H);
    write_series(
        &env,
        "before",
        &machine,
        &[
            keyed(now - 84 * H, None, Some((300, week)), a),
            keyed(now - 12 * H, None, Some((150, week_b)), b),
        ],
    );
    write_series(
        &env,
        "work",
        &machine,
        &[
            keyed(now - 60 * H, None, Some((500, week)), a),
            keyed(now - 39 * H, Some((800, five)), Some((550, week)), a),
            event(now - 38 * H),
            keyed(now - 38 * H, Some((200, five)), Some((100, week_b)), b),
        ],
    );
    write_series(
        &env,
        "now",
        &machine,
        &[reading(now - 6 * H, None, Some((600, week)), None)],
    );
    let theirs = history_of(&env, Some("now"), (false, false), |_| 0);
    let friday = day_lines(&theirs)
        .into_iter()
        .find(|l| l.starts_with("  Fri Sep 19"))
        .unwrap_or_default();
    assert_eq!(
        friday,
        "  Fri Sep 19  week 55% (resets Wed Sep 24 00:00) \u{b7} 5h 1 window, peak 80% \u{b7} \
         another account signed in to the home 10:00",
        "{theirs}"
    );
    assert_eq!(
        dates(&theirs),
        [
            "  Wed Sep 17",
            "  Thu Sep 18",
            "  Fri Sep 19",
            "  Sat Sep 20"
        ],
        "{theirs}"
    );
    for not_a in ["week 15%", "week 10%", "peak 20%", "reset used"] {
        assert!(!theirs.contains(not_a), "{not_a}: {theirs}");
    }
    let mine = history_of(&env, Some("work"), (false, false), |_| 0);
    assert_eq!(dates(&mine), ["  Fri Sep 19", "  Sat Sep 20"], "{mine}");
    for not_b in [
        "week 30%",
        "week 50%",
        "week 55%",
        "peak 80%",
        "reset used",
        "another",
    ] {
        assert!(!mine.contains(not_b), "{not_b}: {mine}");
    }
    assert!(mine.contains("5h 1 window, peak 20%"), "{mine}");
    let by_key = history_of(&env, Some(&a.to_string()), (false, false), |_| 0);
    assert_eq!(by_key, theirs, "the key names the same account");
}

/// A HISTORY READS ONLY THIS MAC'S SERIES. A file of the same account's readings named for
/// another machine (a state folder copied from another Mac) is not this Mac's history, which its
/// header says it is. Negative control: taking every file in the directory (no
/// `is_of_machine` filter in `by_key`) adds the other machine's Friday, and the assertion fails.
#[test]
fn history_reads_only_this_macs_series() {
    let tmp = tmp_dir("history-machine");
    let env = env_at(&tmp);
    let now = env.now;
    let a = AccountKey::claude(UUID).expect("key");
    let machine = pool(&tmp, &env, &["now"], &[("now", a)]);
    let other = if machine == "0123456789abcdef" {
        "fedcba9876543210"
    } else {
        "0123456789abcdef"
    };
    write_series(
        &env,
        "now",
        &machine,
        &[keyed(now - 6 * H, None, Some((600, now + 3 * D)), a)],
    );
    write_series(
        &env,
        "now",
        other,
        &[keyed(now - 30 * H, None, Some((900, now + 3 * D)), a)],
    );
    let text = history_of(&env, Some("now"), (false, false), |_| 0);
    assert_eq!(dates(&text), ["  Sat Sep 20"], "{text}");
    assert!(!text.contains("week 90%"), "{text}");
}

/// A RENAMED DAY IS COUNTED ONCE. The row `before` became `now` at 10:30 on Friday, inside the
/// five-hour window that ended at 11:00 (A read 30% and 50% under the old label, 70% and then
/// 100% under the new one), and the next window ended at 18:00; both files were compacted, so
/// each holds a `day` row of Friday and a `close` of the shared window. Friday is one line: the
/// week from its first figure (40%) to its last (52%), two windows, not three, the shared one
/// full once, and the day's peak. Negative control: each stored `day` row standing over the day
/// in turn (`day.sum` replaced by every stored row, as it was) leaves one file's half of Friday,
/// and the assertion fails.
#[test]
fn a_renamed_day_is_counted_once() {
    let tmp = tmp_dir("history-renamed-day");
    let env = env_at(&tmp);
    let now = env.now;
    let a = AccountKey::claude(UUID).expect("key");
    let machine = pool(&tmp, &env, &["now"], &[("now", a)]);
    let friday = now - 2 * D;
    let (w1, w2, week) = (friday + 11 * H, friday + 18 * H, now + 3 * D);
    let before = write_series(
        &env,
        "before",
        &machine,
        &[
            keyed(friday + 8 * H, Some((300, w1)), Some((400, week)), a),
            keyed(friday + 10 * H, Some((500, w1)), Some((450, week)), a),
        ],
    );
    let after = write_series(
        &env,
        "now",
        &machine,
        &[
            keyed(
                friday + 10 * H + 45 * 60,
                Some((700, w1)),
                Some((480, week)),
                a,
            ),
            keyed(
                friday + 10 * H + 50 * 60,
                Some((1000, w1)),
                Some((500, week)),
                a,
            ),
            keyed(friday + 14 * H, Some((200, w2)), Some((520, week)), a),
        ],
    );
    for path in [&before, &after] {
        account_series::compact_locked(path, now, |_| 0).expect("compact");
        let rows = account_series::read_file(path);
        assert!(
            rows.iter()
                .any(|r| r.t == friday && matches!(r.said, Said::Day(_))),
            "{} holds Friday's day row",
            path.display()
        );
        assert!(
            rows.iter()
                .any(|r| matches!(&r.said, Said::Close(c) if c.reset == w1 && c.w == "five_hour")),
            "{} holds the shared window's close",
            path.display()
        );
    }
    let text = history_of(&env, Some("now"), (false, false), |_| 0);
    assert_eq!(
        day_lines(&text),
        [
            "  Fri Sep 19  week 40% \u{2192} 52% (resets Wed Sep 24 00:00) \u{b7} 5h 2 windows, \
          peak 100%, 1 full"
        ],
        "{text}"
    );
}

/// An hour east of UTC: the zone `history_puts_the_open_window_on_its_rows_day` reads days in.
fn east(_: i64) -> i64 {
    H
}

/// A HISTORY PUTS THE OPEN WINDOW ON ITS ROW'S DAY. A five-hour figure served at 23:55 local
/// and recorded at 00:05, its window still open, is the day of its row, which holds the figure:
/// one line, whose five-hour part says it is open. Negative control: the open window put on the
/// day its figure was served (`local(open.at)`) prints a bare Saturday and a Sunday with no
/// open window, and the assertion fails.
#[test]
fn history_puts_the_open_window_on_its_rows_day() {
    let tmp = tmp_dir("history-open");
    let env = env_at(&tmp);
    let now = env.now;
    let machine = pool(&tmp, &env, &["work"], &[]);
    let served = now - H - 5 * 60;
    let row = SeriesRow {
        t: now - H + 5 * 60,
        by: "m".into(),
        said: Said::Fig {
            five_hour: Some(fig(400, now + 2 * H, served)),
            seven_day: None,
            ac: None,
            key: None,
        },
    };
    write_series(&env, "work", &machine, &[row]);
    let text = history_of(&env, Some("work"), (false, false), east);
    assert_eq!(
        day_lines(&text),
        ["  Sun Sep 21  5h peak 40%, open 40%"],
        "{text}"
    );
}

/// A HISTORY WITHOUT A NAME FINDS A FORGOTTEN ROW'S KEY. The row `now`, keyed by the lab as
/// `ac-…`, has no record here, but the record this Mac forgot under that lab key holds the
/// account key its old series `before` carries: the history of every row finds that series as
/// the named one does. Negative control: the row's lab key not passed to `key_in` (`None`, as
/// it was) finds no account key, and the first assertion fails.
#[test]
fn history_without_a_name_finds_a_forgotten_rows_key() {
    const PK: &str = "ac-0123456789abcdef";
    let tmp = tmp_dir("history-forgotten");
    let env = env_at(&tmp);
    let now = env.now;
    let a = AccountKey::claude(UUID).expect("key");
    let dir = tmp.path().join("logins").join("now");
    login_made_in(&dir);
    let row = accounts::Row {
        label: "now".into(),
        email: Some("now@example.test".into()),
        dir: dir.to_str().map(str::to_owned),
        ..Default::default()
    };
    std::fs::write(
        env.accounts_roster().expect("a config dir"),
        accounts::append_rows_for(Cli::Claude, "", &[row]).expect("the row admits"),
    )
    .expect("write");
    let mut note = None;
    env.accounts_store()
        .transact(env.now, &mut note, |state| {
            state.forgotten.insert(
                PK.into(),
                accounts::Account {
                    key: Some(a),
                    pk: Some(PK.into()),
                    ..Default::default()
                },
            );
            Ok(Vec::new())
        })
        .expect("state");
    let machine = env.accounts_store().load(env.now).expect("load").0.machine;
    write_series(
        &env,
        "before",
        &machine,
        &[keyed(now - 30 * H, None, Some((200, now + 3 * D)), a)],
    );
    crate::harness::journal::with_keyer(
        |_, email| (email == "now@example.test").then(|| PK.to_owned()),
        || {
            let every = history_of(&env, None, (false, false), |_| 0);
            assert_eq!(dates(&every), ["  Fri Sep 19"], "{every}");
            let named = history_of(&env, Some("now"), (false, false), |_| 0);
            assert_eq!(every, named, "the named history finds the same");
        },
    );
}
