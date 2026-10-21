// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the accounts core (`docs/DESIGN-claude-accounts-2026-10-01.md`
//! §4, §5, §6): the roster's admission, the state's reader and its bounds,
//! the condition table at its edges, the advice rule by rule, the
//! transactions, the ledger row and the store.
//!
//! Every clock is the fixed [`NOW`]; every label is invented (`main`,
//! `work-2`, `pool-a`); every path is under a temp dir or a root that does
//! not exist. Nothing here reads a login, a keychain or a Claude home.

use std::collections::BTreeSet;

use aterm_tempfile::TempDir as Tmp;

use super::*;
use crate::harness::host_path::abs;

/// A fixed clock: never `now()`.
const NOW: i64 = 1_790_000_000;
const MIN: i64 = 60;
const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;

fn tmp(tag: &str) -> Tmp {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-harness-accounts-{tag}-"))
        .tempdir()
        .expect("temp dir")
}

fn roster(rows: &[(&str, Class, Option<&str>)]) -> Roster {
    Roster {
        rows: rows
            .iter()
            .map(|(label, class, dir)| Row {
                label: (*label).to_string(),
                class: *class,
                dir: dir.map(str::to_string),
                ..Row::default()
            })
            .collect(),
    }
}

/// The design's own example: this machine's login, a second dedicated
/// account and a shared one.
fn three() -> Roster {
    roster(&[
        ("main", Class::Dedicated, None),
        ("work-2", Class::Dedicated, Some("/logins/work-2")),
        ("pool-a", Class::Shared, Some("/logins/pool-a")),
    ])
}

/// Every row enrolled.
fn all(roster: &Roster) -> BTreeSet<String> {
    roster.rows.iter().map(|r| r.label.clone()).collect()
}

fn state() -> State {
    State {
        machine: "0a0b".to_string(),
        ..State::default()
    }
}

/// A state where `main` is the proven active account.
fn on_main() -> State {
    State {
        active: Some("main".to_string()),
        proven: true,
        ..state()
    }
}

fn me(label: &str) -> Me<'_> {
    Me {
        account: Some(label),
        left: None,
        default_home: true,
        stored_login: true,
    }
}

/// The session a rotation moves, on `account`.
fn mover(account: &str) -> Mover<'_> {
    Mover {
        sid: "s-one",
        account,
    }
}

/// [`begin_rotation`] on [`three`], every row enrolled, by the session
/// [`mover`] names on `by`.
fn rotation(
    s: &mut State,
    read: Option<&str>,
    by: &str,
    to: &str,
    now: i64,
) -> Result<Event, Lost> {
    let r = three();
    begin_rotation(s, &r, &all(&r), read, mover(by), to, now)
}

/// A wall whose row nobody dated: the owner's clearing is not asked about.
fn wall() -> Point {
    Point::Wall {
        window: "five_hour".to_string(),
        at: None,
    }
}

/// A wall whose row was written at `at`.
fn wall_at(at: i64) -> Point {
    Point::Wall {
        window: "five_hour".to_string(),
        at: Some(at),
    }
}

/// A sample read a minute ago, both windows running, far from their resets.
fn sample(session: u32, week: u32) -> Sample {
    Sample {
        session_pct: Some(session),
        session_reset: Some(NOW + 3 * HOUR),
        week_pct: Some(week),
        week_reset: Some(NOW + 3 * DAY),
        at: NOW - MIN,
        by: String::new(),
    }
}

fn fresh(state: &mut State, label: &str) {
    assert_eq!(note_sample(state, label, sample(10, 20), NOW).len(), 1);
}

fn low(state: &mut State, label: &str) {
    assert_eq!(note_sample(state, label, sample(95, 20), NOW).len(), 1);
}

/// A session wall on `label` that stands until `end` (its reset is the
/// grace before that).
fn limited(state: &mut State, label: &str, end: i64) {
    let events = note_wall(
        state,
        label,
        "five_hour",
        Some(end - RESET_GRACE_S),
        NOW - MIN,
        NOW - MIN,
    );
    assert_eq!(events.first().map(|e| e.kind), Some(Kind::Wall));
}

fn cond(roster: &Roster, state: &State, label: &str, now: i64) -> Condition {
    condition(roster.row(label).expect("a row"), state, true, now)
}

fn order(roster: &Roster, state: &State, me: Option<&str>, point: &Point) -> Vec<String> {
    candidates(roster, state, &all(roster), me, point, NOW)
        .into_iter()
        .filter(|(_, verdict)| verdict.is_ok())
        .map(|(label, _)| label)
        .collect()
}

fn why_not(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    point: &Point,
    label: &str,
) -> WhyNot {
    candidates(roster, state, enrolled, Some("main"), point, NOW)
        .into_iter()
        .find(|(l, _)| l == label)
        .and_then(|(_, verdict)| verdict.err())
        .unwrap_or_else(|| panic!("{label} is a candidate"))
}

// ---------------------------------------------------------------------------
// 1. The roster
// ---------------------------------------------------------------------------

/// [`three`] as the owner writes it, its dirs absolute on this host
/// ([`abs`]).
fn reference() -> String {
    format!(
        r#"
# The owner's accounts.
[[account]]
label = "main"             # no dir: the login this machine already has
class = "dedicated"

[[account]]
label = "work-2"
class = "dedicated"
dir = "{}"

[[account]]
label = "pool-a"
class = "shared"
dir = "{}"
"#,
        abs("/logins/work-2"),
        abs("/logins/pool-a")
    )
}

/// [`three`], its dirs as [`reference`] spells them.
fn three_here() -> Roster {
    let (work, pool) = (abs("/logins/work-2"), abs("/logins/pool-a"));
    roster(&[
        ("main", Class::Dedicated, None),
        ("work-2", Class::Dedicated, Some(&work)),
        ("pool-a", Class::Shared, Some(&pool)),
    ])
}

#[test]
fn a_roster_admits_whole_in_file_order_and_a_missing_file_is_the_empty_roster() {
    let parsed = Roster::parse(&reference()).expect("the reference roster admits");
    assert_eq!(parsed, three_here());
    assert_eq!(
        parsed.row("pool-a").map(|r| r.class),
        Some(Class::Shared),
        "a row is found by its label"
    );
    assert!(parsed.row("nobody").is_none());

    // The packaged state: no rows. An empty file, and one of comments only.
    assert_eq!(Roster::parse(""), Ok(Roster::default()));
    assert_eq!(Roster::parse("# nothing listed\n"), Ok(Roster::default()));

    let dir = tmp("roster");
    let path = dir.path().join(ACCOUNTS_FILE);
    assert_eq!(
        Roster::load(&path),
        Ok(Roster::default()),
        "a missing file is not an error"
    );
    std::fs::write(&path, reference()).expect("write");
    assert_eq!(Roster::load(&path), Ok(three_here()));

    // NEGATIVE CASES for the file itself: unreadable, not text, too large.
    assert!(
        Roster::load(dir.path()).is_err(),
        "a directory is not a roster"
    );
    std::fs::write(&path, [0xff, 0xfe, b'\n']).expect("write");
    assert!(Roster::load(&path).is_err(), "not text");
    let big = "# pad\n".repeat(usize::try_from(MAX_ROSTER_BYTES / 6 + 1).expect("fits"));
    std::fs::write(&path, big).expect("write");
    assert!(Roster::load(&path).is_err(), "too large");
}

#[test]
fn every_malformed_roster_is_refused_whole_and_names_the_row_and_the_reason() {
    let ok = "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n";
    // The positive twin every case below is one edit away from.
    assert_eq!(Roster::parse(ok).map(|r| r.rows.len()), Ok(1));
    let second = |tail: &str| format!("{ok}[[account]]\n{tail}");
    let long_label = "a".repeat(LABEL_MAX + 1);
    let long_dir = format!("/{}", "d".repeat(DIR_MAX));
    let cases: Vec<(String, &str)> = vec![
        ("[[account]\nlabel = \"main\"\n".into(), "not valid TOML"),
        (format!("enabled = true\n{ok}"), "unknown key `enabled`"),
        (
            format!("{ok}[policy]\nlimit_reset = true\n"),
            "unknown table `policy`",
        ),
        ("account = \"main\"\n".into(), "`account` is a TOML string"),
        (
            "[account]\nlabel = \"main\"\nclass = \"dedicated\"\n".into(),
            "`account` is a TOML table",
        ),
        ("account = [1]\n".into(), "row 1 is a TOML integer"),
        (
            format!("{ok}priority = 1\n"),
            "row `main` has an unknown key `priority`",
        ),
        (
            "[[account]]\nclass = \"dedicated\"\n".into(),
            "row 1 has no label",
        ),
        (second("class = \"shared\"\n"), "row 2 has no label"),
        (
            "[[account]]\nlabel = 7\nclass = \"dedicated\"\n".into(),
            "row 1: its label is a TOML integer",
        ),
        // The grammar's cases are `admissible_label`'s, below.
        (
            second("label = \"Main\"\nclass = \"shared\"\n"),
            "row 2: label `Main` is outside",
        ),
        (
            second("label = \"help\"\nclass = \"shared\"\ndir = \"/x\"\n"),
            "row 2: label `help` is the harness command's help word",
        ),
        (
            second("label = \"main\"\nclass = \"shared\"\ndir = \"/x\"\n"),
            "label `main` is listed twice (rows 1 and 2)",
        ),
        (
            second("label = \"w\"\nclass = \"team\"\ndir = \"/x\"\n"),
            "row `w`: class `team` is neither",
        ),
        (
            second("label = \"w\"\ndir = \"/x\"\n"),
            "row `w` has no class",
        ),
        (
            second("label = \"w\"\nclass = true\ndir = \"/x\"\n"),
            "row `w`: its class is a TOML boolean",
        ),
        (
            second("label = \"w\"\nclass = \"shared\"\ndir = \"logins/w\"\n"),
            "row `w`: its dir is relative",
        ),
        (
            second("label = \"w\"\nclass = \"shared\"\ndir = \"~/w\"\n"),
            "row `w`: its dir is relative",
        ),
        (
            second("label = \"w\"\nclass = \"shared\"\ndir = 3\n"),
            "row `w`: its dir is a TOML integer",
        ),
        (
            second("label = \"w\"\nclass = \"shared\"\ndir = \"/x\\ty\"\n"),
            "row `w`: its dir holds a control character",
        ),
        (
            second("label = \"w\"\nclass = \"shared\"\ndir = \"/x\\u2028y\"\n"),
            "its dir holds a control character",
        ),
        (
            second(&format!(
                "label = \"w\"\nclass = \"shared\"\ndir = \"{long_dir}\"\n"
            )),
            "its dir is longer than",
        ),
        (
            format!(
                "{}{}",
                second("label = \"w\"\nclass = \"shared\"\ndir = \"/x\"\n"),
                "[[account]]\nlabel = \"v\"\nclass = \"shared\"\ndir = \"/x\"\n"
            ),
            "rows `w` and `v` name one dir",
        ),
        (
            second("label = \"w\"\nclass = \"shared\"\n"),
            "rows `main` and `w` both name no dir",
        ),
    ];
    for (text, why) in &cases {
        assert!(
            Roster::parse(text).is_err(),
            "{why}: {text:?} must be refused"
        );
    }

    // The row bound is a refusal, not a truncation — and its positive twin.
    let many = |n: usize| -> String {
        (0..n)
            .map(|i| {
                let dir = abs(&format!("/x{i}"));
                format!("[[account]]\nlabel = \"a{i}\"\nclass = \"shared\"\ndir = \"{dir}\"\n")
            })
            .collect()
    };
    assert_eq!(
        Roster::parse(&many(MAX_ACCOUNTS)).map(|r| r.rows.len()),
        Ok(MAX_ACCOUNTS)
    );
    assert!(
        Roster::parse(&many(MAX_ACCOUNTS + 1)).is_err(),
        "over the bound"
    );

    // The label grammar, both ways.
    for good in [
        "a",
        "0",
        "main",
        "work-2",
        "pool.a",
        "a_b",
        &"z".repeat(LABEL_MAX),
    ] {
        assert!(admissible_label(good), "{good}");
        assert_eq!(label_refusal(good), None, "{good}");
    }
    for bad in ["", "A", "-a", ".a", "_a", "a b", "a/b", "é", &long_label] {
        assert!(!admissible_label(bad), "{bad}");
        assert!(label_refusal(bad).is_some(), "{bad}");
    }
    // `help` is the one word of the grammar that is no label: the harness
    // command reads it as its help word wherever it stands, so an account
    // named so could be listed and never named. NEGATIVE CONTROL: a label
    // that only begins with it is one.
    assert!(!admissible_label("help"));
    assert!(label_refusal("help").is_some());
    for near in ["helper", "help-2", "h", "hel"] {
        assert!(admissible_label(near), "{near}");
    }
    assert_eq!(
        Store::at(std::path::Path::new("/x")).login_dir("help"),
        None
    );
}

#[test]
fn a_dir_is_used_verbatim_and_two_spellings_of_one_place_are_two_logins() {
    let spellings = [
        "/logins/work 2/",
        "/logins/work 2",
        "/logins/a\"quote\\and-slash",
        "/logins/ünï/",
        "/logins//double",
        "/logins/trailing-space ",
    ]
    .map(abs);
    let spellings = spellings.each_ref().map(String::as_str);
    let mut text = String::new();
    for (i, dir) in spellings.iter().enumerate() {
        let row = row_toml(&format!("w{i}"), Class::Shared, Some(dir)).expect("a row");
        text = append_row(&text, &format!("w{i}"), Class::Shared, Some(dir)).expect("appends");
        assert!(text.ends_with(&row), "append writes the row text");
    }
    let parsed = Roster::parse(&text).expect("the appended file admits");
    assert_eq!(parsed.rows.len(), spellings.len());
    for (row, dir) in parsed.rows.iter().zip(spellings) {
        assert_eq!(row.dir.as_deref(), Some(dir), "byte for byte");
        // The relaunch line carries exactly that string.
        assert_eq!(login_env(row), (LOGIN_ENV, dir.to_string()));
        assert_eq!(parsed.row_for_login(Some(dir)), Some(row));
    }
    // NEGATIVE CONTROL: nothing was canonicalized, so the slash and the
    // space still tell two rows apart.
    assert_ne!(parsed.rows[0].dir, parsed.rows[1].dir);

    // The default login rides as the variable SET AND EMPTY, and is read
    // back as the row with no `dir`.
    let main = Row {
        label: "main".into(),
        class: Class::Dedicated,
        dir: None,
        ..Row::default()
    };
    assert_eq!(
        login_env(&main),
        ("CLAUDE_SECURESTORAGE_CONFIG_DIR", String::new())
    );
    let r = three();
    for unset in [None, Some("")] {
        assert_eq!(
            r.row_for_login(unset).map(|r| r.label.as_str()),
            Some("main")
        );
    }
}

#[test]
fn append_row_writes_only_what_the_next_read_admits() {
    let one = append_row("", "main", Class::Dedicated, None).expect("the first row");
    assert_eq!(
        one,
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n"
    );
    // An existing file with no final line break still gets a clean block.
    let (pool, w) = (abs("/logins/pool-a"), abs("/logins/w"));
    let two =
        append_row(one.trim_end(), "pool-a", Class::Shared, Some(&pool)).expect("the second row");
    assert_eq!(
        Roster::parse(&two).expect("admits").rows,
        roster(&[
            ("main", Class::Dedicated, None),
            ("pool-a", Class::Shared, Some(&pool)),
        ])
        .rows
    );
    assert!(two.starts_with(one.trim_end()), "what was there is kept");

    // NEGATIVE CASES, each by name, and nothing is handed back to write.
    let cases: [(&str, &str, Option<&str>, &str); 3] = [
        (&two, "work-2", Some(&pool), "name one dir"),
        (&two, "work-2", None, "both name no dir"),
        ("oops = 1\n", "work-2", Some(&w), "unknown key `oops`"),
    ];
    for (existing, label, dir, why) in cases {
        assert!(
            append_row(existing, label, Class::Shared, dir).is_err(),
            "{why}"
        );
    }
}

/// ENROLLED MEANS A LOGIN WAS MADE: a row with a `dir` counts only where
/// that directory holds at least one entry ([`login_made`]) — what the
/// vendor's flow writes when it is run with the directory as its home. The
/// directory `accounts add` makes, with nothing in it yet, is LISTED AND NOT
/// ENROLLED: `not-enrolled` in the table. One entry makes it enrolled:
/// `unseen`.
///
/// NEGATIVE CONTROLS: the row with no `dir` — this machine's own login —
/// needs no directory at all; an absent path and a path that is a file are
/// never enrolled; and a directory that cannot be read is not enrolled
/// though it holds a login's file.
#[test]
fn enrolled_is_the_rows_in_whose_login_directory_a_login_was_made() {
    let dir = tmp("enrolled");
    let made = dir.path().join("work-2");
    std::fs::create_dir(&made).expect("mkdir");
    std::fs::write(made.join("a-login-file"), "{}").expect("write");
    let empty = dir.path().join("check-2");
    std::fs::create_dir(&empty).expect("mkdir");
    let file = dir.path().join("a-file");
    std::fs::write(&file, "").expect("write");
    let r = roster(&[
        ("main", Class::Dedicated, None),
        (
            "work-2",
            Class::Dedicated,
            Some(made.to_str().expect("utf-8")),
        ),
        (
            "check-2",
            Class::Dedicated,
            Some(empty.to_str().expect("utf-8")),
        ),
        (
            "pool-a",
            Class::Shared,
            Some(dir.path().join("absent").to_str().expect("utf-8")),
        ),
        ("pool-b", Class::Shared, Some(file.to_str().expect("utf-8"))),
    ]);
    let labels = |r: &Roster| enrolled(r).into_iter().collect::<Vec<String>>();
    assert_eq!(
        labels(&r),
        ["main", "work-2"],
        "this machine's own login always; a directory with a login in it; never an empty \
         directory, an absent path or a file"
    );
    assert!(login_made(&made) && !login_made(&empty));
    assert!(!login_made(&dir.path().join("absent")) && !login_made(&file));
    let s = state();
    let here = enrolled(&r);
    let cond = |label: &str, here: &BTreeSet<String>| {
        let row = r.row(label).expect("listed");
        condition(row, &s, is_enrolled(row, here), NOW)
    };
    assert_eq!(
        cond("main", &here),
        Condition::Unseen,
        "no directory needed"
    );
    assert_eq!(cond("work-2", &here), Condition::Unseen);
    for label in ["check-2", "pool-a", "pool-b"] {
        assert_eq!(cond(label, &here), Condition::NotEnrolled, "{label}");
    }

    // ONE ENTRY makes it enrolled: unseen.
    std::fs::write(empty.join("a-login-file"), "{}").expect("the login is made");
    let here = enrolled(&r);
    assert_eq!(labels(&r), ["check-2", "main", "work-2"]);
    assert_eq!(cond("check-2", &here), Condition::Unseen);

    // NEGATIVE CONTROL: a directory nobody may read is not enrolled, whatever
    // it holds. (Skipped for a user no mode stops.)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |bits: u32| {
            std::fs::set_permissions(&made, std::fs::Permissions::from_mode(bits)).expect("chmod");
        };
        mode(0o000);
        if std::fs::read_dir(&made).is_err() {
            assert!(!login_made(&made), "unreadable: not enrolled");
            assert_eq!(labels(&r), ["check-2", "main"]);
        }
        mode(0o700);
        assert!(login_made(&made));
    }
}

// ---------------------------------------------------------------------------
// 2. The state
// ---------------------------------------------------------------------------

/// A state with every field set.
fn full_state() -> State {
    let mut s = on_main();
    s.machine = "00112233aabbccdd".to_string();
    limited(&mut s, "main", NOW + HOUR);
    assert!(!note_wall(&mut s, "main", "seven_day", None, NOW - 5, NOW - 5).is_empty());
    fresh(&mut s, "work-2");
    assert_eq!(
        note_sample(
            &mut s,
            "work-2",
            Sample {
                session_pct: None,
                session_reset: None,
                week_pct: Some(41),
                week_reset: Some(NOW + DAY),
                at: NOW - 1,
                by: String::new(),
            },
            NOW
        )
        .len(),
        1
    );
    assert!(note_offer(
        &mut s,
        "main",
        None,
        2,
        Some(NOW + 2 * DAY),
        NOW - 9,
        NOW
    ));
    assert_eq!(
        note_auth(&mut s, "pool-a", Auth::SignedOut, NOW - 7).len(),
        1
    );
    assert_eq!(note_auth(&mut s, "work-2", Auth::Ok, NOW - 7).len(), 1);
    assert!(note_session(&mut s, "s-1a2b", "main", Some("jobs"), NOW - 3).is_empty());
    assert!(note_session(&mut s, "s-3c4d", "work-2", None, NOW - 2).is_empty());
    s.previous = Some("pool-a".to_string());
    s.previous_proven = true;
    s.pinned = true;
    s.accounts.entry("main".into()).or_default().cleared_at = Some(NOW - 20);
    s.rotations = vec![NOW - 100, NOW - 50];
    assert!(claim_probe(&mut s, "main", "s-1a2b", NOW - 4).is_some());
    s.reset = Some(ResetClaim {
        account: "main".to_string(),
        until: Some(NOW + HOUR),
        window: Some(WEEK_LIMIT.to_owned()),
        sid: "s-1a2b".to_string(),
        at: NOW - 4,
        pid: 4242,
        start: "1790000000.250000".to_string(),
        done: Some("kept".to_string()),
    });
    s
}

#[test]
fn the_state_round_trips_through_json_with_sorted_keys_and_null_for_unknown() {
    let s = full_state();
    let text = s.to_json();
    assert!(!text.contains('\n'), "one line");
    assert_eq!(State::from_json(&text, NOW), Ok(s.clone()));

    // Keys are sorted at every level, so two writers of one state agree
    // byte for byte.
    let doc: Value = aterm_json::from_str(&text).expect("JSON");
    let top: Vec<&str> = doc
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        top,
        [
            "accounts",
            "active",
            "canary",
            "machine",
            "pinned",
            "previous",
            "previous_proven",
            "probe",
            "proven",
            "reset",
            "rotations",
            "schema",
            "sessions",
            "undo"
        ]
    );
    assert_eq!(doc["schema"].as_u64(), Some(SCHEMA));

    // Unknown is `null`, never `0`: a sample with no session figure, a wall
    // with no reset, a session nothing holds.
    assert!(doc["accounts"]["work-2"]["sample"]["session_pct"].is_null());
    assert!(doc["accounts"]["work-2"]["sample"]["session_reset"].is_null());
    assert_eq!(
        doc["accounts"]["work-2"]["sample"]["week_pct"].as_u64(),
        Some(41)
    );
    assert!(doc["accounts"]["main"]["walls"]["seven_day"]["until"].is_null());
    assert!(doc["sessions"]["s-3c4d"]["held"].is_null());
    assert!(doc["accounts"]["pool-a"]["sample"].is_null());
    // The keys another build reads the previous sample and the owner's
    // clearing back by.
    assert_eq!(
        doc["accounts"]["work-2"]["before"]["session_pct"].as_u64(),
        Some(10)
    );
    assert_eq!(
        doc["accounts"]["main"]["cleared_at"].as_i64(),
        Some(NOW - 20)
    );

    // The reset claim with its process and its outcome.
    assert_eq!(
        aterm_json::to_string(&doc["reset"]).expect("text"),
        format!(
            "{{\"account\":\"main\",\"at\":{},\"done\":\"kept\",\"pid\":4242,\
             \"sid\":\"s-1a2b\",\"start\":\"1790000000.250000\",\"until\":{},\"window\":\"seven_day\"}}",
            NOW - 4,
            NOW + HOUR
        )
    );
    // A FILE AN OLDER BUILD WROTE has none of those keys, and is the same
    // schema: it loads, with no account on record as run on and the claim
    // read as no process's and not done (it lapses by its own clock).
    // The object at `path` of a state's JSON, to edit.
    fn at<'a>(doc: &'a mut Value, path: &[&str]) -> &'a mut Map {
        path.iter()
            .fold(doc, |v, key| {
                v.as_object_mut()
                    .and_then(|o| o.get_mut(*key))
                    .expect("a key")
            })
            .as_object_mut()
            .expect("an object")
    }
    let mut old: Value = aterm_json::from_str(&text).expect("JSON");
    for label in ["main", "work-2", "pool-a"] {
        assert!(
            at(&mut old, &["accounts", label])
                .remove("ran_at")
                .is_some(),
            "{label}"
        );
    }
    for key in ["pid", "start", "done", "window"] {
        assert!(at(&mut old, &["reset"]).remove(key).is_some(), "{key}");
    }
    let read = State::from_json(&aterm_json::to_string(&old).expect("text"), NOW).expect("loads");
    let mut want = s.clone();
    for account in want.accounts.values_mut() {
        account.ran_at = None;
    }
    want.reset = Some(ResetClaim {
        window: None,
        pid: 0,
        start: String::new(),
        done: None,
        ..s.reset.clone().expect("the claim")
    });
    assert_eq!(read, want);
    // NEGATIVE CONTROL: the keys were what differed.
    assert_ne!(read, s);

    // The empty state round-trips too.
    let empty = state();
    assert_eq!(State::from_json(&empty.to_json(), NOW), Ok(empty));

    // …and one in the middle of a rotation: the canary, the proof that
    // travels with the account that was left, and what the rotation wrote
    // over (here the record of a session that had taken a canary's place).
    let mid = State {
        active: Some("work-2".into()),
        previous: Some("main".into()),
        previous_proven: true,
        proven: false,
        canary: Some(Canary {
            sid: "s-one".into(),
            at: NOW - 30,
            took: false,
        }),
        undo: vec![
            Undo::default(),
            Undo {
                previous: Some("pool-a".into()),
                previous_proven: true,
                canary: Some(Canary {
                    sid: "s-zero".into(),
                    at: NOW - 900,
                    took: true,
                }),
            },
        ],
        ..state()
    };
    let text = mid.to_json();
    assert_eq!(State::from_json(&text, NOW), Ok(mid.clone()));
    let read = |doc: &Value| {
        State::from_json(&aterm_json::to_string(doc).expect("text"), NOW).expect("reads")
    };
    // A file written before the undo was kept: nothing to take back, and
    // the schema is the same one.
    let mut doc: Value = aterm_json::from_str(&text).expect("JSON");
    let top = doc.as_object_mut().expect("object");
    assert!(top.remove("undo").is_some());
    assert_eq!(
        read(&doc),
        State {
            undo: Vec::new(),
            ..mid.clone()
        }
    );
    // A file written before those keys existed reads them as the weaker
    // word: the account left was NOT proven, and no session holds the
    // canary's place.
    let top = doc.as_object_mut().expect("object");
    assert!(top.remove("previous_proven").is_some() && top.remove("canary").is_some());
    assert_eq!(
        read(&doc),
        State {
            previous_proven: false,
            canary: None,
            undo: Vec::new(),
            ..mid.clone()
        }
    );
    // An undo is its rotation's maker's to use, so it is read only beside a
    // canary that made a rotation: beside one that took another's place, or
    // none, it is nobody's and is dropped.
    for (canary, kept) in [
        (aterm_json::json!({"sid": "s-one", "at": NOW - 30}), 2),
        (
            aterm_json::json!({"sid": "s-one", "at": NOW - 30, "took": true}),
            0,
        ),
        (Value::Null, 0),
    ] {
        let mut doc: Value = aterm_json::from_str(&text).expect("JSON");
        doc.as_object_mut()
            .expect("object")
            .insert("canary".into(), canary.clone());
        assert_eq!(read(&doc).undo.len(), kept, "{canary:?}");
    }
}

#[test]
fn unknown_keys_are_tolerated_and_a_file_this_build_must_not_guess_at_is_refused() {
    let s = full_state();
    let mut doc: Value = aterm_json::from_str(&s.to_json()).expect("JSON");
    // A newer writer's additions, at every level.
    let top = doc.as_object_mut().expect("object");
    top.insert(
        "service".into(),
        aterm_json::json!({"url": "x", "n": [1, 2]}),
    );
    top.insert("next_field".into(), Value::from(7_i64));
    let main = top
        .get_mut("accounts")
        .and_then(|a| a.as_object_mut())
        .and_then(|a| a.get_mut("main"))
        .and_then(|m| m.as_object_mut())
        .expect("main");
    main.insert("plan".into(), Value::from("big"));
    main.get_mut("walls")
        .and_then(|w| w.as_object_mut())
        .and_then(|w| w.get_mut("five_hour"))
        .and_then(|w| w.as_object_mut())
        .expect("a wall")
        .insert("model".into(), Value::from("x"));
    let text = aterm_json::to_string(&doc).expect("text");
    assert_eq!(
        State::from_json(&text, NOW),
        Ok(s.clone()),
        "extra keys change nothing"
    );
    // What this build must not guess at (torn, or not ours) is the store's
    // tests', through `Store::load`.
}

/// A STAMP AHEAD OF THE CLOCK IS NOT NEWER THAN EVERYTHING. A state written
/// while the clock ran a day fast (or merged from a machine whose clock
/// does) is read with every observation's `at` — and the auth word's and
/// the pin's — brought back to the reader's clock: `now` plus the skew
/// allowed for another machine's observation, `now` for this machine's own.
/// The instants that are MEANT to be ahead (a wall's reset, a window's, an
/// offer's deadline) are left alone.
#[test]
fn a_stamp_ahead_of_the_clock_is_read_as_made_no_later_than_now_plus_the_skew() {
    let ahead = NOW + DAY;
    let mut s = state();
    let main = s.accounts.entry("main".into()).or_default();
    main.walls.insert(
        "five_hour".into(),
        Wall {
            until: Some(ahead + HOUR),
            at: ahead,
            by: "0a0b".into(),
            attempts: 0,
            cleared: false,
        },
    );
    main.sample = Some(Sample {
        at: ahead,
        ..sample(10, 20)
    });
    main.before = Some(Sample {
        at: NOW - HOUR,
        ..sample(5, 20)
    });
    main.offer = Some(Offer {
        kind: Some(aterm_phase::ResetKind::Full),
        left: 1,
        use_by: Some(ahead + DAY),
        at: ahead,
        by: "0a0b".into(),
    });
    main.auth = Auth::SignedOut;
    main.auth_at = Some(ahead);
    main.cleared_at = Some(ahead);
    main.ran_at = Some(ahead);
    let text = s.to_json();

    let read = State::from_json(&text, NOW).expect("reads");
    let main = &read.accounts["main"];
    let cap = NOW + STAMP_SKEW_S;
    assert_eq!(main.walls["five_hour"].at, cap);
    assert_eq!(main.sample.as_ref().map(|x| x.at), Some(cap));
    assert_eq!(main.offer.as_ref().map(|x| x.at), Some(cap));
    // This machine's own stamps have no other clock to allow for.
    assert_eq!(
        (main.auth_at, main.cleared_at, main.ran_at),
        (Some(NOW), Some(NOW), Some(NOW))
    );
    // So the next check's answer is recorded: against the stamp as it was
    // written, every word would be "older" until the clock caught up.
    let mut checked = read.clone();
    assert_eq!(note_auth(&mut checked, "main", Auth::Ok, NOW).len(), 1);
    assert_eq!(checked.accounts["main"].auth, Auth::Ok);
    let mut unread = s.clone();
    assert!(
        note_auth(&mut unread, "main", Auth::Ok, NOW).is_empty(),
        "NEGATIVE CONTROL: the unread stamp shadows it"
    );
    // What was not ahead is as it was, and so is every instant that is not
    // a stamp.
    assert_eq!(main.before.as_ref().map(|x| x.at), Some(NOW - HOUR));
    assert_eq!(main.walls["five_hour"].until, Some(ahead + HOUR));
    assert_eq!(
        main.offer.as_ref().and_then(|x| x.use_by),
        Some(ahead + DAY)
    );
    assert_eq!(
        main.sample.as_ref().and_then(|x| x.session_reset),
        Some(NOW + 3 * HOUR)
    );
    // NEGATIVE CONTROL: read once the clock has caught up, nothing moves.
    assert_eq!(State::from_json(&text, ahead), Ok(s));
}

#[test]
fn a_hostile_state_is_bounded_as_it_is_read() {
    let long = "x".repeat(10_000);
    let mut accounts = aterm_json::Map::new();
    for i in 0..(MAX_ACCOUNTS + 20) {
        let mut walls = aterm_json::Map::new();
        for w in 0..(MAX_WALLS + 12) {
            walls.insert(
                format!("window_{w:02}"),
                aterm_json::json!({"until": null, "at": w, "by": long.clone(), "attempts": 5_000_000_000_u64}),
            );
        }
        accounts.insert(
            format!("a{i:03}"),
            aterm_json::json!({
                "walls": Value::Object(walls),
                "sample": {"session_pct": 5000, "week_pct": -3, "at": 9, "by": 12},
                "before": "not an object",
                "offer": {"left": "many", "at": 1},
                "auth": "root",
                "auth_at": "yesterday",
            }),
        );
    }
    accounts.insert("Not A Label".into(), aterm_json::json!({"auth": "ok"}));
    accounts.insert(long.clone(), aterm_json::json!({"auth": "ok"}));
    accounts.insert("zz-not-an-object".into(), Value::from(3_i64));
    let mut sessions = aterm_json::Map::new();
    for i in 0..(MAX_SESSIONS + 44) {
        sessions.insert(
            format!("s-{i:04}"),
            aterm_json::json!({"account": "a000", "at": i, "held": long.clone()}),
        );
    }
    sessions.insert(
        "s-bad-account".into(),
        aterm_json::json!({"account": "No Such", "at": 1}),
    );
    sessions.insert("s-no-time".into(), aterm_json::json!({"account": "a000"}));
    let rotations: Vec<Value> = (0..(MAX_ROTATIONS + 72))
        .map(|i| Value::from(i64::try_from(i).expect("fits")))
        .collect();
    let undo: Vec<Value> = (0..(MAX_UNDO + 5))
        .map(|i| {
            aterm_json::json!({
                "previous": if i % 2 == 0 { "a000" } else { "Not A Label" },
                "previous_proven": "yes",
                "canary": {"sid": long.clone(), "at": i},
            })
        })
        .chain([Value::from(3_i64), Value::from("not an object")])
        .collect();
    let doc = aterm_json::json!({
        "schema": 1,
        "machine": "not hex at all",
        "accounts": Value::Object(accounts),
        "active": long.clone(),
        "previous": 4,
        "proven": "yes",
        "pinned": 1,
        "canary": {"sid": long.clone(), "at": 7},
        "undo": Value::Array(undo),
        "sessions": Value::Object(sessions),
        "rotations": Value::Array(rotations),
        "probe": {"account": "a000", "sid": long.clone(), "at": 3},
        "reset": {"account": "a000", "sid": "s-1"},
    });
    let s = State::from_json(&aterm_json::to_string(&doc).expect("text"), NOW).expect("reads");

    assert_eq!(s.machine, "", "an id that is not hex is no id");
    assert_eq!(s.accounts.len(), MAX_ACCOUNTS);
    assert!(s.accounts.keys().all(|k| admissible_label(k)));
    let a = &s.accounts["a000"];
    assert_eq!(a.walls.len(), MAX_WALLS);
    assert!(
        a.walls.values().all(|w| w.at >= 12),
        "the newest observations are the ones kept"
    );
    assert!(a.walls.values().all(|w| w.by.len() <= WORD_MAX));
    assert!(
        a.walls.values().all(|w| w.attempts == u32::MAX),
        "saturated, not wrapped"
    );
    let sample = a.sample.as_ref().expect("a sample with a time");
    assert_eq!(
        (sample.session_pct, sample.week_pct),
        (None, None),
        "no figure, never 0"
    );
    assert_eq!(sample.by, "");
    assert_eq!(a.before, None);
    assert_eq!(a.offer, None);
    assert_eq!(
        (a.auth, a.auth_at),
        (Auth::Unseen, None),
        "never a stronger word"
    );
    assert_eq!((s.active.as_deref(), s.previous.as_deref()), (None, None));
    assert!(!s.proven && !s.pinned);
    assert_eq!(s.sessions.len(), MAX_SESSIONS);
    assert!(
        s.sessions.values().all(|row| row.at >= 44),
        "the newest sessions are the ones kept"
    );
    assert!(
        s.sessions
            .values()
            .all(|row| row.held.as_ref().is_some_and(|h| h.len() <= WORD_MAX))
    );
    assert!(!s.sessions.contains_key("s-bad-account") && !s.sessions.contains_key("s-no-time"));
    assert_eq!(s.rotations.len(), MAX_ROTATIONS);
    assert_eq!(
        s.rotations.first(),
        Some(&72),
        "the newest times are the ones kept"
    );
    assert_eq!(s.probe.as_ref().map(|c| c.sid.len()), Some(WORD_MAX));
    assert_eq!(s.reset, None, "a claim with no time is no claim");
    assert_eq!(s.canary.as_ref().map(|c| c.sid.len()), Some(WORD_MAX));
    assert_eq!(s.undo.len(), MAX_UNDO, "the newest undos are the ones kept");
    assert_eq!(
        s.undo.first().and_then(|u| u.canary.as_ref()).map(|c| c.at),
        Some(5)
    );
    assert!(s.undo.iter().all(|u| {
        !u.previous_proven
            && u.previous.as_deref().is_none_or(|l| l == "a000")
            && u.canary.as_ref().is_some_and(|c| c.sid.len() == WORD_MAX)
    }));
    assert!(s.to_json().len() < 200_000, "what is kept is small");
}

#[test]
fn the_transactions_keep_every_map_bounded() {
    let mut s = state();
    for i in 0..(MAX_ACCOUNTS + 5) {
        let at = NOW + i64::try_from(i).expect("fits");
        assert_eq!(
            note_auth(&mut s, &format!("a{i:02}"), Auth::Ok, at).len(),
            1
        );
    }
    assert_eq!(s.accounts.len(), MAX_ACCOUNTS);
    assert!(
        !s.accounts.contains_key("a00"),
        "the one written longest ago went"
    );
    assert!(
        s.accounts
            .contains_key(&format!("a{:02}", MAX_ACCOUNTS + 4))
    );

    for w in 0..(MAX_WALLS + 3) {
        let until = NOW + i64::try_from(w).expect("fits") * HOUR;
        assert_eq!(
            note_wall(&mut s, "a10", &format!("w{w}"), Some(until), NOW, NOW).len(),
            1
        );
    }
    assert_eq!(s.accounts["a10"].walls.len(), MAX_WALLS);
    assert!(
        !s.accounts["a10"].walls.contains_key("w0"),
        "the most lapsed wall went"
    );

    for i in 0..(MAX_SESSIONS + 9) {
        let at = NOW + i64::try_from(i).expect("fits");
        note_session(&mut s, &format!("s-{i:04}"), "a10", None, at);
    }
    assert_eq!(s.sessions.len(), MAX_SESSIONS);
    assert!(!s.sessions.contains_key("s-0000") && s.sessions.contains_key("s-0009"));

    // The undo: a rotation past an account that had not answered and
    // cannot be run on keeps what it wrote over, and the oldest goes.
    let r = roster(&[
        ("a10", Class::Dedicated, None),
        ("a11", Class::Dedicated, Some("/logins/a11")),
    ]);
    let mut chained = State {
        active: Some("a10".into()),
        canary: Some(Canary {
            sid: "s-zero".into(),
            at: NOW,
            took: false,
        }),
        undo: (0..MAX_UNDO)
            .map(|i| Undo {
                previous_proven: i == 0,
                ..Undo::default()
            })
            .collect(),
        ..s.clone()
    };
    begin_rotation(
        &mut chained,
        &r,
        &all(&r),
        Some("a10"),
        mover("a10"),
        "a11",
        NOW,
    )
    .expect("a10 is limited, and is rotated past");
    assert_eq!(chained.undo.len(), MAX_UNDO);
    assert!(
        chained.undo.iter().all(|u| !u.previous_proven),
        "the oldest went"
    );
    assert_eq!(
        chained
            .undo
            .last()
            .and_then(|u| u.canary.as_ref())
            .map(|c| c.sid.as_str()),
        Some("s-zero")
    );

    // What no roster could carry is never written.
    let before = s.clone();
    assert!(note_wall(&mut s, "Not A Label", "five_hour", None, NOW, NOW).is_empty());
    assert!(note_wall(&mut s, "a10", " \n ", None, NOW, NOW).is_empty());
    assert!(note_sample(&mut s, "../x", sample(1, 1), NOW).is_empty());
    assert!(!note_offer(&mut s, "", None, 1, None, NOW, NOW));
    assert!(note_auth(&mut s, "A", Auth::Ok, NOW).is_empty());
    assert!(note_session(&mut s, "", "a10", None, NOW).is_empty());
    assert!(note_session(&mut s, "s-1", "A B", None, NOW).is_empty());
    assert_eq!(claim_probe(&mut s, "A", "s-1", NOW), None);
    assert!(strike(&mut s, "A", "s-1", &Strike::SignedOut, NOW).is_empty());
    assert_eq!(s, before);
}

// ---------------------------------------------------------------------------
// 3. The condition
// ---------------------------------------------------------------------------

#[test]
fn the_condition_table_in_its_order() {
    let r = three();
    let mut s = state();
    let row = r.row("work-2").expect("a row");

    // Never observed.
    assert_eq!(condition(row, &s, true, NOW), Condition::Unseen);
    // No login directory beats everything known about the account.
    limited(&mut s, "work-2", NOW + HOUR);
    assert_eq!(condition(row, &s, false, NOW), Condition::NotEnrolled);
    // Signed out beats a standing wall.
    assert_eq!(
        condition(row, &s, true, NOW),
        Condition::Limited { until: NOW + HOUR }
    );
    note_auth(&mut s, "work-2", Auth::SignedOut, NOW);
    assert_eq!(condition(row, &s, true, NOW), Condition::SignedOut);
    note_auth(&mut s, "work-2", Auth::Ok, NOW + 1);
    // A standing wall beats a low sample.
    low(&mut s, "work-2");
    assert_eq!(
        condition(row, &s, true, NOW),
        Condition::Limited { until: NOW + HOUR }
    );
    // The wall over, the low sample speaks.
    assert_eq!(condition(row, &s, true, NOW + HOUR), Condition::Low);

    // Observed with nothing against it: usable. A login check alone is an
    // observation.
    let mut s = state();
    note_auth(&mut s, "pool-a", Auth::Ok, NOW);
    assert_eq!(cond(&r, &s, "pool-a", NOW), Condition::Usable);
    fresh(&mut s, "main");
    assert_eq!(cond(&r, &s, "main", NOW), Condition::Usable);

    // The word is `accounts --json`'s `condition` field.
    for (c, word, fresh, runs) in [
        (Condition::NotEnrolled, "not-enrolled", false, false),
        (Condition::SignedOut, "signed-out", false, false),
        (Condition::Limited { until: 1 }, "limited", false, false),
        (Condition::Low, "low", false, true),
        (Condition::Usable, "usable", true, true),
        (Condition::Unseen, "unseen", true, true),
    ] {
        assert_eq!((c.as_str(), c.fresh(), c.runs()), (word, fresh, runs));
    }
}

/// [`signed_out`]: the ENROLLED rows whose last word is signed out (roster
/// order, and a sign-in ending one, are the host's look's test). The row with
/// no `dir` is enrolled whatever the set holds. NEGATIVE CONTROLS: a limited
/// row is left out, and so is a label the roster does not list.
#[test]
fn the_signed_out_labels_are_the_enrolled_ones_in_roster_order() {
    let r = three();
    let mut s = state();
    for label in ["main", "work-2", "pool-a"] {
        note_auth(&mut s, label, Auth::SignedOut, NOW);
    }
    assert_eq!(
        signed_out(&r, &s, &all(&r), NOW),
        ["main", "work-2", "pool-a"]
    );
    // NEGATIVE CONTROL: signed out and not enrolled is `not-enrolled`; the row
    // with no `dir` is enrolled whatever the set holds.
    let here: BTreeSet<String> = ["work-2".to_string()].into();
    assert_eq!(
        condition(r.row("pool-a").expect("a row"), &s, false, NOW),
        Condition::NotEnrolled
    );
    assert_eq!(signed_out(&r, &s, &here, NOW), ["main", "work-2"]);
    assert_eq!(signed_out(&r, &s, &BTreeSet::new(), NOW), ["main"]);

    // NEGATIVE CONTROL: a limited row is not a signed-out one, and a label
    // the roster does not list is nobody's.
    let mut s = state();
    limited(&mut s, "work-2", NOW + HOUR);
    low(&mut s, "pool-a");
    note_auth(&mut s, "gone-9", Auth::SignedOut, NOW);
    assert!(matches!(
        cond(&r, &s, "work-2", NOW),
        Condition::Limited { .. }
    ));
    assert_eq!(signed_out(&r, &s, &all(&r), NOW), Vec::<String>::new());
}

/// [`sign_in_next`]: the first OTHER row in roster order that is not signed
/// in here — never enrolled, or signed out — with whether it signs in by
/// renewal (an email) or `accounts login` (the review's acct-1, acct-4).
/// NEGATIVE CONTROLS: the session's own row is never named, a limited row is
/// no row to sign in, and every other row signed in names none.
#[test]
fn the_row_to_sign_in_next_is_the_first_other_one_not_signed_in() {
    let mut r = three();
    let here: BTreeSet<String> = ["work-2".to_string()].into();
    // pool-a never enrolled: named, by `accounts login` (no email).
    assert_eq!(
        sign_in_next(&r, &state(), &here, Some("main"), NOW),
        Some(("pool-a".to_string(), false))
    );
    // With an email, by renewal.
    r.rows[2].email = Some("pool@example.com".to_string());
    assert_eq!(
        sign_in_next(&r, &state(), &here, Some("main"), NOW),
        Some(("pool-a".to_string(), true))
    );
    // Signed out comes first in roster order.
    let mut s = state();
    note_auth(&mut s, "work-2", Auth::SignedOut, NOW);
    assert_eq!(
        sign_in_next(&r, &s, &here, Some("main"), NOW),
        Some(("work-2".to_string(), false))
    );
    // NEGATIVE CONTROL: the session's own row is never the one to sign in.
    assert_eq!(
        sign_in_next(&r, &s, &here, Some("work-2"), NOW),
        Some(("pool-a".to_string(), true))
    );
    // NEGATIVE CONTROL: every other row signed in (a limited one included).
    let mut s = state();
    limited(&mut s, "work-2", NOW + HOUR);
    assert_eq!(sign_in_next(&r, &s, &all(&r), Some("main"), NOW), None);
}

#[test]
fn a_wall_stands_until_its_instant_and_not_a_second_longer() {
    let r = three();
    let wall = |until: Option<i64>, at: i64, attempts: u32| Wall {
        until,
        at,
        by: String::new(),
        attempts,
        cleared: false,
    };
    let reset = NOW + HOUR;
    // (the wall, the instant it stops standing)
    let cases = [
        // Named, observed before its reset: the reset plus the grace.
        (wall(Some(reset), NOW, 0), reset + 150),
        (wall(Some(reset), reset, 0), reset + 150),
        // Observed AFTER its own reset: 10 minutes, then 30, from the look.
        (wall(Some(reset), reset + 1, 1), reset + 1 + 10 * MIN),
        (wall(Some(reset), reset + 200, 1), reset + 200 + 10 * MIN),
        (wall(Some(reset), reset + 200, 2), reset + 200 + 30 * MIN),
        (wall(Some(reset), reset + 200, 9), reset + 200 + 30 * MIN),
        // Naming no reset: 30 minutes, 2 hours, 6 hours.
        (wall(None, NOW, 1), NOW + 30 * MIN),
        (wall(None, NOW, 2), NOW + 2 * HOUR),
        (wall(None, NOW, 3), NOW + 6 * HOUR),
        (wall(None, NOW, 40), NOW + 6 * HOUR),
        // A count of zero reads as the first rung, never an index below it.
        (wall(None, NOW, 0), NOW + 30 * MIN),
    ];
    for (w, end) in cases {
        assert_eq!(w.stands_until(), Some(end), "{w:?}");
        let mut s = state();
        s.accounts
            .entry("work-2".into())
            .or_default()
            .walls
            .insert("five_hour".into(), w.clone());
        assert_eq!(
            cond(&r, &s, "work-2", end - 1),
            Condition::Limited { until: end },
            "one second before: {w:?}"
        );
        assert_eq!(
            cond(&r, &s, "work-2", end),
            Condition::Usable,
            "at its instant: {w:?}"
        );
    }
    // Two walls: the account is limited until the LAST one ends.
    let mut s = state();
    note_wall(&mut s, "work-2", "five_hour", Some(NOW + HOUR), NOW, NOW);
    note_wall(&mut s, "work-2", "seven_day", Some(NOW + 2 * DAY), NOW, NOW);
    assert_eq!(
        cond(&r, &s, "work-2", NOW + HOUR + 150),
        Condition::Limited {
            until: NOW + 2 * DAY + 150
        }
    );
    // A hostile instant saturates instead of wrapping.
    assert_eq!(wall(Some(i64::MAX), 0, 0).stands_until(), Some(i64::MAX));
    assert_eq!(wall(None, i64::MAX, 3).stands_until(), Some(i64::MAX));
}

#[test]
fn low_is_a_running_window_at_its_threshold_however_near_its_reset() {
    let r = three();
    let at = |s: Sample, now: i64| {
        let mut st = state();
        st.accounts.entry("work-2".into()).or_default().sample = Some(s);
        cond(&r, &st, "work-2", now)
    };

    // The thresholds, one percent either side.
    assert_eq!(at(sample(90, 0), NOW), Condition::Low);
    assert_eq!(
        at(sample(89, 96), NOW),
        Condition::Usable,
        "NEGATIVE CONTROL"
    );
    assert_eq!(at(sample(0, 97), NOW), Condition::Low);
    assert_eq!(at(sample(100, 100), NOW), Condition::Low);

    // LOW IS ABOUT THE ACCOUNT: at its threshold it is low until that
    // window ends, however near the reset — what is left of the window is
    // still all there is. (That it is about to be replaced is a reason not
    // to LEAVE it early, which is `leave_early`'s and rule 4's.)
    let session = |reset: i64| Sample {
        session_reset: Some(reset),
        ..sample(95, 0)
    };
    for reset in [NOW + 901, NOW + 900, NOW + 1] {
        assert_eq!(at(session(reset), NOW), Condition::Low, "{}", reset - NOW);
        assert_eq!(session(reset).low_until(NOW), Some(reset));
    }
    let week = |reset: i64| Sample {
        week_reset: Some(reset),
        ..sample(0, 99)
    };
    assert_eq!(at(week(NOW + 901), NOW), Condition::Low);
    assert_eq!(at(week(NOW + 900), NOW), Condition::Low);
    // The guard, where it belongs: a session is worth moving off only an
    // account whose low window is MORE than 15 minutes from its reset (the
    // session window's edge is rule 4's test).
    assert!(week(NOW + 901).leave_early(NOW));
    assert!(!week(NOW + 900).leave_early(NOW));
    assert!(!sample(89, 96).leave_early(NOW), "not low at all");
    // Two low windows: the account is low until the LAST of them ends, and
    // the one far from its reset is reason enough to leave.
    let two = Sample {
        session_reset: Some(NOW + 5 * MIN),
        ..sample(95, 99)
    };
    assert_eq!(two.low_until(NOW), Some(NOW + 3 * DAY));
    assert!(two.leave_early(NOW));
    assert_eq!(sample(89, 96).low_until(NOW), None);

    // A window whose reset has passed voids its figure: the same sample
    // read later says nothing about the window that replaced it.
    let s = session(NOW + HOUR);
    assert_eq!(s.session(NOW + HOUR - 1), Some((95, NOW + HOUR)));
    assert_eq!(s.session(NOW + HOUR), None);
    assert_eq!(at(s.clone(), NOW + HOUR - 901), Condition::Low);
    assert_eq!(at(s.clone(), NOW + HOUR), Condition::Usable);
    assert_eq!(at(s, NOW + 2 * HOUR), Condition::Usable);
    let w = week(NOW + DAY);
    assert_eq!(
        (w.week(NOW + DAY - 1), w.week(NOW + DAY)),
        (Some((99, NOW + DAY)), None)
    );
    // …while the other window, still running, keeps its say.
    let both = Sample {
        session_reset: Some(NOW + HOUR),
        ..sample(95, 99)
    };
    assert_eq!(
        at(both, NOW + 2 * HOUR),
        Condition::Low,
        "the week still runs"
    );

    // A figure whose reset nobody read is void one window length after its
    // sample, and low to the last second before that bound.
    let unread = Sample {
        session_reset: None,
        ..sample(95, 0)
    };
    let end = unread.at + SESSION_WINDOW_S;
    assert_eq!(at(unread.clone(), end - 901), Condition::Low);
    assert_eq!(at(unread.clone(), end - 1), Condition::Low);
    assert_eq!(unread.low_until(end - 1), Some(end));
    assert_eq!(at(unread, end), Condition::Usable);

    // A figure nobody read is never low: unknown is not zero and not full.
    let none = Sample {
        session_pct: None,
        week_pct: None,
        ..sample(0, 0)
    };
    assert_eq!(at(none, NOW), Condition::Usable);
}

#[test]
fn one_wall_is_one_observation_until_it_is_found_standing_again() {
    let mut s = state();
    let reset = NOW + HOUR;
    let attempts = |s: &State, window: &str| s.accounts["main"].walls[window].attempts;

    // Ten tabs at one wall write one row.
    let first = note_wall(&mut s, "main", "five_hour", Some(reset), NOW, NOW);
    assert_eq!(first.len(), 1);
    assert_eq!(
        (first[0].kind, first[0].t, first[0].account.as_str()),
        (Kind::Wall, NOW, "main")
    );
    for tab in 1..10 {
        assert!(
            note_wall(
                &mut s,
                "main",
                "five_hour",
                Some(reset),
                NOW + tab,
                NOW + tab
            )
            .is_empty()
        );
    }
    assert_eq!(attempts(&s, "five_hour"), 0);
    assert_eq!(
        s.accounts["main"].walls["five_hour"].by, "0a0b",
        "stamped with this machine"
    );
    // …up to the last second it stands.
    assert!(
        note_wall(
            &mut s,
            "main",
            "five_hour",
            Some(reset),
            reset + 149,
            reset + 149
        )
        .is_empty()
    );

    // Found standing after it should have lifted: 10 minutes from the look,
    // then 30, and 30 for ever.
    let look1 = reset + 150;
    assert_eq!(
        note_wall(&mut s, "main", "five_hour", Some(reset), look1, look1).len(),
        1
    );
    assert_eq!(attempts(&s, "five_hour"), 1);
    assert_eq!(
        s.accounts["main"].limited_until(look1),
        Some(look1 + 10 * MIN)
    );
    assert!(
        note_wall(
            &mut s,
            "main",
            "five_hour",
            Some(reset),
            look1 + 10 * MIN - 1,
            look1 + 10 * MIN - 1
        )
        .is_empty()
    );
    let look2 = look1 + 10 * MIN;
    assert_eq!(
        note_wall(&mut s, "main", "five_hour", Some(reset), look2, look2).len(),
        1
    );
    assert_eq!(
        s.accounts["main"].limited_until(look2),
        Some(look2 + 30 * MIN)
    );
    let look3 = look2 + 30 * MIN;
    assert_eq!(
        note_wall(&mut s, "main", "five_hour", Some(reset), look3, look3).len(),
        1
    );
    assert_eq!(
        s.accounts["main"].limited_until(look3),
        Some(look3 + 30 * MIN)
    );

    // A NEW wall (another reset) starts over; first seen late, it is on the
    // first rung.
    let next = reset + 5 * HOUR;
    assert_eq!(
        note_wall(
            &mut s,
            "main",
            "five_hour",
            Some(next),
            look3 + 1,
            look3 + 1
        )
        .len(),
        1
    );
    assert_eq!(attempts(&s, "five_hour"), 0);
    assert_eq!(
        note_wall(&mut s, "main", "seven_day", Some(NOW), NOW + 5, NOW + 5).len(),
        1
    );
    assert_eq!(attempts(&s, "seven_day"), 1, "observed after its own reset");

    // An observation older than the one held is dropped: one made at `NOW`
    // and only handed in now, with the clock where the newer one left it.
    assert!(note_wall(&mut s, "main", "five_hour", Some(reset), NOW, look3 + 1).is_empty());
    assert_eq!(s.accounts["main"].walls["five_hour"].until, Some(next));

    // A wall naming no reset climbs 30 minutes, 2 hours, 6 hours — one rung
    // per time it is found standing, never per tab.
    let mut s = state();
    let mut look = NOW;
    for (n, rung) in [(1, 30 * MIN), (2, 2 * HOUR), (3, 6 * HOUR), (4, 6 * HOUR)] {
        let events = note_wall(&mut s, "main", "overage", None, look, look);
        assert_eq!(events.len(), 1, "look {n}");
        assert!(
            note_wall(&mut s, "main", "overage", None, look + 1, look + 1).is_empty(),
            "a second tab"
        );
        assert_eq!(attempts(&s, "overage"), n);
        assert_eq!(s.accounts["main"].limited_until(look), Some(look + rung));
        look += rung;
    }
    // Seen again long after it stopped standing, it is a new wall.
    let much_later = look + WALL_EPISODE_S + 1;
    assert_eq!(
        note_wall(&mut s, "main", "overage", None, much_later, much_later).len(),
        1
    );
    assert_eq!(attempts(&s, "overage"), 1);
}

// ---------------------------------------------------------------------------
// 4. Advice
// ---------------------------------------------------------------------------

#[test]
fn rule_1_stays_for_a_session_the_roster_does_not_govern() {
    let r = three();
    let none = Roster::default();
    let mut s = on_main();
    limited(&mut s, "main", NOW + HOUR);
    let enrolled = all(&r);
    // The positive twin: this session, at this wall, moves.
    assert!(matches!(
        advise(&r, &s, &enrolled, &me("main"), &wall(), Mode::Early, NOW),
        Advice::Move { .. }
    ));
    let unlisted = Me {
        account: None,
        ..me("main")
    };
    let other_home = Me {
        default_home: false,
        ..me("main")
    };
    let api_key = Me {
        stored_login: false,
        ..me("main")
    };
    for (roster, who, mode, why) in [
        (&r, &me("main"), Mode::Off, StayWhy::Off),
        (&none, &me("main"), Mode::Early, StayWhy::NoRoster),
        (&r, &unlisted, Mode::Early, StayWhy::NotListed),
        (&r, &me("gone"), Mode::Early, StayWhy::NotListed),
        (&r, &other_home, Mode::Early, StayWhy::OtherHome),
        (&r, &api_key, Mode::Wall, StayWhy::NotAStoredLogin),
    ] {
        assert_eq!(
            advise(roster, &s, &enrolled, who, &wall(), mode, NOW),
            Advice::Stay(why),
            "{why:?}"
        );
    }
}

/// Rule 2 follows a proven, fresh active account — and not one whose login is
/// known signed out or that is no longer enrolled (neither is the model's).
#[test]
fn rule_2_follows_no_active_account_signed_out_or_not_enrolled() {
    let r = three();
    let enrolled = all(&r);
    let mut s = on_main();
    s.active = Some("work-2".into());
    let ask = |s: &State, enrolled: &BTreeSet<String>| {
        advise(
            &r,
            s,
            enrolled,
            &me("main"),
            &Point::TurnEnd,
            Mode::Wall,
            NOW,
        )
    };
    // The positive twin: followed at a plain turn end, in `wall` mode too.
    assert_eq!(
        ask(&s, &enrolled),
        Advice::Move {
            to: "work-2".into(),
            ahead: true,
            rotates: false,
            takes_canary: false
        }
    );
    let mut out = s.clone();
    note_auth(&mut out, "work-2", Auth::SignedOut, NOW);
    assert_eq!(ask(&out, &enrolled), Advice::Stay(StayWhy::NothingDue));
    let mut gone = enrolled.clone();
    gone.remove("work-2");
    assert_eq!(ask(&s, &gone), Advice::Stay(StayWhy::NothingDue));
}

/// A PROVEN ACTIVE ACCOUNT THAT IS LOW, the hour's rotations spent: a session
/// at its wall falls back on it — unless an account opens within 15 minutes,
/// this session's own included (the model has no clock). And the
/// compare-and-set never rotates past a low active account for another low
/// one, nor past a pinned one.
#[test]
fn a_low_active_account_is_fallen_back_on_and_not_rotated_past_for_a_low_one() {
    let r = three();
    let enrolled = all(&r);
    let mut at_wall = on_main();
    at_wall.active = Some("work-2".into());
    low(&mut at_wall, "work-2");
    limited(&mut at_wall, "main", NOW + 3 * HOUR);
    let ask = |s: &State| advise(&r, s, &enrolled, &me("main"), &wall(), Mode::Wall, NOW);

    let spent = State {
        rotations: vec![NOW - 5; 6],
        ..at_wall.clone()
    };
    assert_eq!(
        ask(&spent),
        Advice::Move {
            to: "work-2".into(),
            ahead: false,
            rotates: false,
            takes_canary: false
        }
    );
    let mut near = spent.clone();
    near.accounts.remove("main");
    limited(&mut near, "main", NOW + 5 * MIN);
    assert!(matches!(ask(&near), Advice::Stay(StayWhy::Breaker { .. })));

    let commit = |s: &State| {
        let who = mover("main");
        begin_rotation(
            &mut s.clone(),
            &r,
            &enrolled,
            Some("work-2"),
            who,
            "pool-a",
            NOW,
        )
    };
    let held = Err(Lost::Held {
        active: "work-2".into(),
    });
    let mut all_low = at_wall.clone();
    low(&mut all_low, "pool-a");
    assert_eq!(commit(&all_low), held);
    let pinned = State {
        pinned: true,
        ..at_wall.clone()
    };
    assert_eq!(commit(&pinned), held);
    // NEGATIVE CONTROL: past it to a fresh account, it commits.
    assert!(commit(&at_wall).is_ok());
}

#[test]
fn candidates_sort_by_one_key_fresh_dedicated_soonest_week_label() {
    let week = |reset: Option<i64>| Sample {
        week_reset: reset,
        ..sample(10, 20)
    };
    let seen = |s: &mut State, label: &str, reset: Option<i64>| {
        assert_eq!(note_sample(s, label, week(reset), NOW).len(), 1);
    };
    let d = Class::Dedicated;
    let sh = Class::Shared;

    // Dedicated before shared, whatever the file order.
    let r = roster(&[
        ("pool-a", sh, Some("/p")),
        ("main", d, None),
        ("work-2", d, Some("/w")),
    ]);
    let mut s = state();
    for label in ["pool-a", "work-2"] {
        fresh(&mut s, label);
    }
    assert_eq!(order(&r, &s, Some("main"), &wall()), ["work-2", "pool-a"]);

    // The week that resets soonest; unknown last; a reset already passed is
    // unknown too.
    let r = roster(&[
        ("b-late", d, Some("/b")),
        ("a-none", d, Some("/a")),
        ("d-past", d, Some("/d")),
        ("c-soon", d, Some("/c")),
        ("main", d, None),
    ]);
    let mut s = state();
    seen(&mut s, "b-late", Some(NOW + 5 * DAY));
    seen(&mut s, "a-none", None);
    seen(&mut s, "d-past", Some(NOW));
    seen(&mut s, "c-soon", Some(NOW + DAY));
    assert_eq!(
        order(&r, &s, Some("main"), &wall()),
        ["c-soon", "b-late", "a-none", "d-past"]
    );

    // The label breaks the last tie, so the order is the roster's and not
    // the file's.
    let r = roster(&[
        ("zed", sh, Some("/z")),
        ("alpha", sh, Some("/a")),
        ("main", d, None),
    ]);
    assert_eq!(order(&r, &state(), Some("main"), &wall()), ["alpha", "zed"]);

    // The whole list comes back: the candidates best first, then every
    // other row in roster order, each with its reason.
    let r = three();
    let mut s = state();
    limited(&mut s, "work-2", NOW + HOUR);
    let listed = candidates(&r, &s, &all(&r), Some("main"), &wall(), NOW);
    assert_eq!(
        listed,
        vec![
            (
                "pool-a".to_string(),
                Ok(Rank {
                    low: false,
                    class: Class::Shared,
                    week_reset: None
                })
            ),
            ("main".to_string(), Err(WhyNot::Me)),
            (
                "work-2".to_string(),
                Err(WhyNot::Limited { until: NOW + HOUR })
            ),
        ]
    );
    // The report asks for no session: no row is `me`.
    assert_eq!(
        order(&r, &state(), None, &wall()),
        ["main", "work-2", "pool-a"]
    );
}

#[test]
fn every_non_candidate_has_its_reason_by_name() {
    let r = three();
    let mut enrolled = all(&r);
    let mut s = state();

    assert_eq!(why_not(&r, &s, &enrolled, &wall(), "main"), WhyNot::Me);
    limited(&mut s, "work-2", NOW + HOUR);
    assert_eq!(
        why_not(&r, &s, &enrolled, &wall(), "work-2"),
        WhyNot::Limited { until: NOW + HOUR }
    );
    note_auth(&mut s, "pool-a", Auth::SignedOut, NOW);
    assert_eq!(
        why_not(&r, &s, &enrolled, &wall(), "pool-a"),
        WhyNot::SignedOut
    );
    enrolled.remove("pool-a");
    assert_eq!(
        why_not(&r, &s, &enrolled, &wall(), "pool-a"),
        WhyNot::NotEnrolled
    );

    // LOW: a candidate only at a wall…
    let enrolled = all(&r);
    let mut s = state();
    low(&mut s, "work-2");
    assert_eq!(
        why_not(&r, &s, &enrolled, &Point::TurnEnd, "work-2"),
        WhyNot::LowAheadOfAWall
    );
    // …and only when no account becomes usable within 15 minutes — this
    // session's own included.
    limited(&mut s, "pool-a", NOW + LOW_ONLY_AFTER_S + 1);
    assert_eq!(
        order(&r, &s, Some("main"), &wall()),
        ["work-2"],
        "nobody is near: it is used"
    );
    let mut near = s.clone();
    near.accounts.remove("pool-a");
    limited(&mut near, "pool-a", NOW + LOW_ONLY_AFTER_S);
    assert_eq!(
        why_not(&r, &near, &enrolled, &wall(), "work-2"),
        WhyNot::LowWhileOneIsNear {
            at: NOW + LOW_ONLY_AFTER_S
        }
    );
    assert!(order(&r, &near, Some("main"), &wall()).is_empty());
    let mut mine = s.clone();
    limited(&mut mine, "main", NOW + 5 * MIN);
    assert_eq!(
        why_not(&r, &mine, &enrolled, &wall(), "work-2"),
        WhyNot::LowWhileOneIsNear { at: NOW + 5 * MIN }
    );
    // The wait is then for that near account, never for `now`.
    assert_eq!(
        advise(&r, &mine, &enrolled, &me("main"), &wall(), Mode::Wall, NOW),
        Advice::Wait {
            until: NOW + 5 * MIN
        }
    );
}

/// AN ACCOUNT AT ITS THRESHOLD IS NEVER A FRESH CANDIDATE, however near its
/// reset. The 15-minute guard is not part of `low`: otherwise an account with
/// NOTHING left, ten minutes from its reset, reads as `usable` and the best
/// fresh candidate, and a session spends a rotation to walk into a wall.
#[test]
fn a_spent_account_about_to_reset_is_low_and_never_a_fresh_candidate() {
    let r = three();
    let enrolled = all(&r);
    let spent = |pct: u32| Sample {
        session_reset: Some(NOW + 10 * MIN),
        ..sample(pct, 20)
    };
    let world = |pct: u32| {
        let mut s = on_main();
        low(&mut s, "main");
        assert_eq!(note_sample(&mut s, "work-2", spent(pct), NOW).len(), 1);
        fresh(&mut s, "pool-a");
        s
    };
    let s = world(100);
    let to = |label: &str, ahead: bool| Advice::Move {
        to: label.into(),
        ahead,
        rotates: true,
        takes_canary: false,
    };
    let ask = |s: &State, point: &Point, now: i64| {
        advise(&r, s, &enrolled, &me("main"), point, Mode::Early, now)
    };
    assert_eq!(cond(&r, &s, "work-2", NOW), Condition::Low);

    // A plain turn end on `main` (95 %): the move ahead of the wall goes to
    // the account with budget, not to the dedicated one with none.
    assert_eq!(ask(&s, &Point::TurnEnd, NOW), to("pool-a", true));
    assert_eq!(
        why_not(&r, &s, &enrolled, &Point::TurnEnd, "work-2"),
        WhyNot::LowAheadOfAWall
    );

    // At a wall on `main` it is not even a low candidate: its own reset is
    // the account "usable within 15 minutes".
    let mut walled = s.clone();
    limited(&mut walled, "main", NOW + 3 * HOUR);
    assert_eq!(order(&r, &walled, Some("main"), &wall()), ["pool-a"]);
    assert_eq!(
        why_not(&r, &walled, &enrolled, &wall(), "work-2"),
        WhyNot::LowWhileOneIsNear { at: NOW + 10 * MIN }
    );
    assert_eq!(ask(&walled, &wall(), NOW), to("pool-a", false));

    // With nowhere else to go the wall WAITS for that reset — the instant
    // the account is fresh again — and takes it then.
    note_auth(&mut walled, "pool-a", Auth::SignedOut, NOW);
    assert_eq!(
        next_usable(&r, &walled, &enrolled, NOW),
        Some(NOW + 10 * MIN)
    );
    assert_eq!(
        ask(&walled, &wall(), NOW),
        Advice::Wait {
            until: NOW + 10 * MIN
        }
    );
    assert_eq!(ask(&walled, &wall(), NOW + 10 * MIN), to("work-2", false));

    // NEGATIVE CONTROL: the same account one percent under its threshold is
    // fresh, and — dedicated — the first candidate.
    assert_eq!(ask(&world(89), &Point::TurnEnd, NOW), to("work-2", true));
}

#[test]
fn the_wait_is_for_the_earliest_instant_and_prefers_a_dedicated_one_minutes_later() {
    let r = three();
    let enrolled = all(&r);
    let until = |main: i64, work: i64, pool: i64| {
        let mut s = state();
        limited(&mut s, "main", NOW + main);
        limited(&mut s, "work-2", NOW + work);
        limited(&mut s, "pool-a", NOW + pool);
        next_usable(&r, &s, &enrolled, NOW).map(|t| t - NOW)
    };
    // The earliest of all.
    assert_eq!(until(3 * HOUR, 2 * HOUR, 4 * HOUR), Some(2 * HOUR));
    // A shared account opens first, a dedicated one within 15 minutes of
    // it: wait for the dedicated one.
    assert_eq!(
        until(3 * HOUR, HOUR + 10 * MIN, HOUR),
        Some(HOUR + 10 * MIN)
    );
    assert_eq!(
        until(3 * HOUR, HOUR + DEDICATED_WAIT_S, HOUR),
        Some(HOUR + DEDICATED_WAIT_S)
    );
    // NEGATIVE CONTROL: one second past the 15 minutes, the earliest wins.
    assert_eq!(
        until(3 * HOUR, HOUR + DEDICATED_WAIT_S + 1, HOUR),
        Some(HOUR)
    );
    // The earliest dedicated one of two.
    assert_eq!(
        until(HOUR + 5 * MIN, HOUR + 10 * MIN, HOUR),
        Some(HOUR + 5 * MIN)
    );

    // What has no instant: signed out, not enrolled. Fresh is `now`. A LOW
    // account's is the end of the window that makes it low, when it is
    // fresh again — here its session window, three hours off.
    let mut s = state();
    limited(&mut s, "main", NOW + HOUR);
    note_auth(&mut s, "work-2", Auth::SignedOut, NOW);
    low(&mut s, "pool-a");
    assert_eq!(next_usable(&r, &s, &enrolled, NOW), Some(NOW + HOUR));
    note_auth(&mut s, "main", Auth::SignedOut, NOW);
    assert_eq!(next_usable(&r, &s, &enrolled, NOW), Some(NOW + 3 * HOUR));
    let mut gone = enrolled.clone();
    gone.remove("pool-a");
    assert_eq!(next_usable(&r, &s, &gone, NOW), None, "not enrolled");
    assert_eq!(next_usable(&r, &state(), &enrolled, NOW), Some(NOW));
    let mut nobody = state();
    for label in ["main", "work-2", "pool-a"] {
        note_auth(&mut nobody, label, Auth::SignedOut, NOW);
    }
    assert_eq!(next_usable(&r, &nobody, &enrolled, NOW), None);
    assert_eq!(
        next_usable(&Roster::default(), &state(), &enrolled, NOW),
        None
    );

    // And `advise` hands that instant to the tab.
    let mut s = on_main();
    limited(&mut s, "main", NOW + 3 * HOUR);
    limited(&mut s, "work-2", NOW + HOUR + 10 * MIN);
    limited(&mut s, "pool-a", NOW + HOUR);
    assert_eq!(
        advise(&r, &s, &enrolled, &me("main"), &wall(), Mode::Early, NOW),
        Advice::Wait {
            until: NOW + HOUR + 10 * MIN
        }
    );
}

/// An account holding a reset offer about to expire is NOT left early: the
/// reset is offered only at a wall. (Rule 4 itself is the model's.)
#[test]
fn rule_4_does_not_leave_an_account_whose_reset_offer_is_about_to_expire() {
    let r = three();
    let enrolled = all(&r);
    let mut s = on_main();
    low(&mut s, "main");
    let ahead = Advice::Move {
        to: "work-2".into(),
        ahead: true,
        rotates: true,
        takes_canary: false,
    };
    let held = Advice::Stay(StayWhy::OfferExpiring);
    for (left, use_by, want, why) in [
        (1, Some(NOW + DAY), &held, "a day off"),
        (1, Some(NOW + 1), &held, "a second off"),
        (1, Some(NOW + DAY + 1), &ahead, "more than a day off"),
        (1, Some(NOW), &ahead, "already expired"),
        (0, Some(NOW + HOUR), &ahead, "no reset left"),
        (1, None, &ahead, "a deadline nobody read is not one"),
    ] {
        let mut s = s.clone();
        assert!(note_offer(&mut s, "main", None, left, use_by, NOW - 5, NOW));
        let advice = advise(
            &r,
            &s,
            &enrolled,
            &me("main"),
            &Point::TurnEnd,
            Mode::Early,
            NOW,
        );
        assert_eq!(&advice, want, "{why}");
    }
}

/// WHO IS LEFT EARLY. The 15-minute guard is about leaving: a session is not
/// moved off an account whose low window is about to reset. A wall with no
/// sample at all is left as a low account is, and no `/usage` is typed first;
/// a reset offer about to expire holds it as it holds a low one.
#[test]
fn rule_4_leaves_a_limited_account_and_not_one_whose_low_window_is_about_to_reset() {
    let r = three();
    let enrolled = all(&r);
    let ask = |s: &State| {
        advise(
            &r,
            s,
            &enrolled,
            &me("main"),
            &Point::TurnEnd,
            Mode::Early,
            NOW,
        )
    };
    let ahead = Advice::Move {
        to: "work-2".into(),
        ahead: true,
        rotates: true,
        takes_canary: false,
    };

    // The guard, to the second — on an account that is `low` either side.
    let resets = |at: i64| {
        let mut s = on_main();
        let read = Sample {
            session_reset: Some(at),
            ..sample(95, 20)
        };
        assert_eq!(note_sample(&mut s, "main", read, NOW).len(), 1);
        assert_eq!(cond(&r, &s, "main", NOW), Condition::Low);
        s
    };
    assert_eq!(ask(&resets(NOW + RESET_SOON_S + 1)), ahead);
    assert_eq!(
        ask(&resets(NOW + RESET_SOON_S)),
        Advice::Stay(StayWhy::NothingDue)
    );

    let mut bare = on_main();
    limited(&mut bare, "main", NOW + 3 * HOUR);
    assert_eq!(ask(&bare), ahead);
    // NEGATIVE CONTROL: the same account with no wall is probed and stays.
    assert_eq!(ask(&on_main()), Advice::Probe);
    let mut offered = bare.clone();
    assert!(note_offer(
        &mut offered,
        "main",
        None,
        1,
        Some(NOW + HOUR),
        NOW,
        NOW
    ));
    assert_eq!(ask(&offered), Advice::Stay(StayWhy::OfferExpiring));
}

#[test]
fn rule_4_probes_when_the_sample_is_due_and_only_then() {
    let r = three();
    let enrolled = all(&r);
    let ask = |r: &Roster, s: &State, mode: Mode, now: i64| {
        advise(r, s, &enrolled, &me("main"), &Point::TurnEnd, mode, now)
    };
    // Never sampled: due at once.
    let s = on_main();
    assert_eq!(ask(&r, &s, Mode::Early, NOW), Advice::Probe);
    // NEGATIVE CONTROLS: the probe is `early`'s alone; it is never typed at
    // a wall; and a roster with nowhere to move to probes nothing.
    assert_eq!(
        ask(&r, &s, Mode::Wall, NOW),
        Advice::Stay(StayWhy::NothingDue)
    );
    assert_ne!(
        advise(&r, &s, &enrolled, &me("main"), &wall(), Mode::Early, NOW),
        Advice::Probe
    );
    let alone = roster(&[("main", Class::Dedicated, None)]);
    assert_eq!(
        ask(&alone, &s, Mode::Early, NOW),
        Advice::Stay(StayWhy::NothingDue)
    );

    // Sampled a minute ago with no burn known: due ten minutes after the
    // sample, to the second.
    let mut s = on_main();
    fresh(&mut s, "main");
    let at = NOW - MIN;
    assert_eq!(
        ask(&r, &s, Mode::Early, at + PROBE_DEFAULT_S - 1),
        Advice::Stay(StayWhy::NothingDue)
    );
    assert_eq!(
        ask(&r, &s, Mode::Early, at + PROBE_DEFAULT_S),
        Advice::Probe
    );

    // One probe per account per machine: a claim younger than the floor
    // holds the others off, on that account only.
    let due = at + PROBE_DEFAULT_S;
    let claimed = claim_probe(&mut s, "main", "s-1", due).expect("the first claim");
    assert_eq!(
        (claimed.kind, claimed.sid.as_deref()),
        (Kind::Probe, Some("s-1"))
    );
    assert_eq!(
        claim_probe(&mut s, "main", "s-2", due + 1),
        None,
        "the second session loses"
    );
    assert_eq!(
        ask(&r, &s, Mode::Early, due + PROBE_MIN_S - 1),
        Advice::Stay(StayWhy::NothingDue)
    );
    assert_eq!(
        ask(&r, &s, Mode::Early, due + PROBE_MIN_S),
        Advice::Probe,
        "a claim nobody kept lapses"
    );
    assert!(claim_probe(&mut s, "main", "s-2", due + PROBE_MIN_S).is_some());

    // The probe goes before the early move: a low account whose sample is
    // stale is read again first.
    let mut s = on_main();
    low(&mut s, "main");
    assert_eq!(ask(&r, &s, Mode::Early, at + PROBE_MAX_S), Advice::Probe);
}

/// THE CLAIM'S SPACING ([`PROBE_APART_S`], [`probe_held`]): no account is
/// probed within 90 s of ANY account's claim, and the same account not within
/// its own floor, twice as long — which the ONE claim field keeps through
/// another account's claim written over it. A claim stamped ahead of the
/// clock holds nobody off. (The 90 s and the floor themselves are asked at
/// `advise`, below and above.)
#[test]
fn no_account_is_probed_within_ninety_seconds_of_any_accounts_claim() {
    let t = NOW;
    let claimed = || {
        let mut s = on_main();
        assert!(claim_probe(&mut s, "work-2", "s-two", t).is_some());
        s
    };
    // The floor through another account's claim: `work-2` at t, `main` at
    // t+90 (it writes over the one field), and `work-2` again is held off by
    // THAT claim until t+180 — its own floor, to the second.
    let mut s = claimed();
    assert!(claim_probe(&mut s, "main", "s-one", t + 90).is_some());
    assert_eq!(claim_probe(&mut s, "work-2", "s-two", t + 179), None);
    assert!(claim_probe(&mut s, "work-2", "s-two", t + 180).is_some());

    // A claim stamped ahead of the clock holds nobody off, of either kind.
    let mut s = on_main();
    s.probe = Some(ProbeClaim {
        account: "work-2".into(),
        sid: "s-x".into(),
        at: t + DAY,
    });
    assert!(!probe_held(&s, "main", t) && !probe_held(&s, "work-2", t));
    // …and no claim at all holds nobody off.
    assert!(!probe_held(&on_main(), "main", t));
}

/// `advise` AND THE CLAIM SHARE ONE TEST: under another account's fresh
/// claim the probe is not advised — the claim would be refused — and the
/// rule goes on to the leave check, which moves a session off a low account
/// ahead of its wall. NEGATIVE CONTROL: with no claim the same state is
/// advised the probe (the low account's sample is stale: read again first),
/// and so it is once the other account's claim is ninety seconds old.
#[test]
fn under_another_accounts_fresh_claim_the_leave_check_answers() {
    let r = three();
    let enrolled = all(&r);
    let ask = |s: &State, now: i64| {
        advise(
            &r,
            s,
            &enrolled,
            &me("main"),
            &Point::TurnEnd,
            Mode::Early,
            now,
        )
    };
    let ahead = Advice::Move {
        to: "work-2".into(),
        ahead: true,
        rotates: true,
        takes_canary: false,
    };
    // `main` is low, its one sample twenty minutes old: the probe is due.
    let stale = NOW - MIN + PROBE_MAX_S;
    let mut s = on_main();
    low(&mut s, "main");
    assert_eq!(cond(&r, &s, "main", stale), Condition::Low);
    assert_eq!(ask(&s, stale), Advice::Probe, "the control: no claim");

    let mut claimed = s.clone();
    assert!(claim_probe(&mut claimed, "pool-a", "s-pool", stale - 10).is_some());
    assert_eq!(ask(&claimed, stale), ahead);
    // …and the claim it would have made is refused, as the advice implies.
    assert_eq!(
        claim_probe(&mut claimed.clone(), "main", "s-one", stale),
        None
    );
    // To the second: at ninety the probe is advised again, and taken.
    assert_eq!(ask(&claimed, stale - 10 + PROBE_APART_S - 1), ahead);
    assert_eq!(ask(&claimed, stale - 10 + PROBE_APART_S), Advice::Probe);
    assert!(claim_probe(&mut claimed, "main", "s-one", stale - 10 + PROBE_APART_S).is_some());
    // An account with nothing to leave stays, under the claim, where it was
    // probed without one.
    let mut fresh_main = on_main();
    assert_eq!(ask(&fresh_main, NOW), Advice::Probe);
    assert!(claim_probe(&mut fresh_main, "pool-a", "s-pool", NOW - 10).is_some());
    assert_eq!(ask(&fresh_main, NOW), Advice::Stay(StayWhy::NothingDue));
}

/// THE UNPROVEN ACTIVE ACCOUNT IS NEVER PROBED: the session on the account
/// a rotation chose, before that account has served a response, is the
/// canary (or stands where one stood) — what a `/usage` read shows there is
/// not known to be that account's, and the rotation's model reads no sample
/// of it. NEGATIVE CONTROL: proven, the same account in the same state is
/// probed.
#[test]
fn the_unproven_active_account_is_not_probed() {
    let r = three();
    let enrolled = all(&r);
    let ask = |s: &State, label: &str, now: i64| {
        advise(
            &r,
            s,
            &enrolled,
            &me(label),
            &Point::TurnEnd,
            Mode::Early,
            now,
        )
    };
    let unproven = |s: &State| State {
        active: Some("work-2".to_string()),
        proven: false,
        canary: Some(Canary {
            sid: "s-two".to_string(),
            at: NOW - 30,
            took: false,
        }),
        ..s.clone()
    };
    let proven = |s: &State| State {
        active: Some("work-2".to_string()),
        proven: true,
        ..s.clone()
    };

    // Never sampled: due at once, were it proven.
    let bare = state();
    assert_eq!(ask(&proven(&bare), "work-2", NOW), Advice::Probe);
    assert_eq!(
        ask(&unproven(&bare), "work-2", NOW),
        Advice::Stay(StayWhy::NothingDue)
    );
    // …and no claim would be made: the advice is what a host claims on.

    // Low, its sample stale: proven it is read again first; unproven it is
    // neither probed nor left (the canary's own stay).
    let stale = NOW - MIN + PROBE_MAX_S;
    let mut low_two = state();
    low(&mut low_two, "work-2");
    assert_eq!(ask(&proven(&low_two), "work-2", stale), Advice::Probe);
    assert_eq!(
        ask(&unproven(&low_two), "work-2", stale),
        Advice::Stay(StayWhy::Canary)
    );
    // The hold lapsed changes nothing for the session ON the account: it
    // has still not answered.
    let lapsed = State {
        canary: None,
        ..unproven(&bare)
    };
    assert_eq!(
        ask(&lapsed, "work-2", NOW),
        Advice::Stay(StayWhy::NothingDue)
    );
}

#[test]
fn the_probe_interval_is_half_the_time_to_the_threshold_clamped() {
    let pair = |old: Sample, new: Sample| Account {
        sample: Some(new),
        before: Some(old),
        ..Account::default()
    };
    let at = |pct_s: u32, pct_w: u32, at: i64| Sample {
        at,
        ..sample(pct_s, pct_w)
    };
    // No sample, one sample: no burn is known.
    assert_eq!(probe_interval(&Account::default()), 600);
    let one = Account {
        sample: Some(sample(50, 50)),
        ..Account::default()
    };
    assert_eq!(probe_interval(&one), 600);

    // 50 % → 60 % of the session in 10 minutes is 1 % a minute; 30 % to go
    // is 30 minutes; half is 15.
    assert_eq!(
        probe_interval(&pair(at(50, 0, NOW - 600), at(60, 0, NOW))),
        900
    );
    // Fast: 60 → 80 in 10 minutes; 10 % to go is 5 minutes; half is 2.5,
    // clamped up to 3.
    assert_eq!(
        probe_interval(&pair(at(60, 0, NOW - 600), at(80, 0, NOW))),
        180
    );
    // Slow: 10 → 11 in 10 minutes; 79 % to go; clamped down to 20 minutes.
    assert_eq!(
        probe_interval(&pair(at(10, 0, NOW - 600), at(11, 0, NOW))),
        1200
    );
    // The week counts too, and the sooner of the two decides: 90 → 94 in 10
    // minutes; 3 % to go is 7.5 minutes; half is 225 s.
    assert_eq!(
        probe_interval(&pair(at(10, 90, NOW - 600), at(11, 94, NOW))),
        225
    );
    // Already at the threshold: the floor.
    assert_eq!(
        probe_interval(&pair(at(80, 0, NOW - 600), at(92, 0, NOW))),
        180
    );
    // A burn of zero is known: the threshold is never reached, so the cap.
    assert_eq!(
        probe_interval(&pair(at(40, 40, NOW - 600), at(40, 40, NOW))),
        1200
    );

    // A window that rolled over between the two samples says nothing; with
    // neither window comparable, no burn is known.
    let rolled = Sample {
        session_reset: Some(NOW + 5 * HOUR),
        week_reset: Some(NOW + 7 * DAY),
        ..at(2, 1, NOW)
    };
    assert_eq!(probe_interval(&pair(at(80, 80, NOW - 600), rolled)), 600);
    // Two samples at one instant give no rate.
    assert_eq!(probe_interval(&pair(at(50, 0, NOW), at(60, 0, NOW))), 600);

    // Due at the interval, to the second. A sample stamped ahead of the
    // clock (a clock set back) says nothing about now: it is due.
    let a = pair(at(50, 0, NOW - 600), at(60, 0, NOW));
    assert!(!probe_due(&a, NOW), "the instant it was read");
    assert!(!probe_due(&a, NOW + 899));
    assert!(probe_due(&a, NOW + 900));
    assert!(probe_due(&a, NOW - 5));
    assert!(probe_due(&a, NOW - DAY));
    assert!(probe_due(&Account::default(), NOW), "never sampled");
}

/// THE OWNER'S PIN (`accounts use`): the account's walls are cleared and it is
/// active, proven and pinned; the compare-and-set holds it; `use --auto` ends
/// it with a row, and a login found signed out ends it too. Refused by name
/// for a label not listed, not enrolled or signed out. (Who follows it, and
/// that rule 4 is off under it, are the model's.)
#[test]
fn the_owners_pin_clears_the_walls_and_holds_the_compare_and_set() {
    let r = three();
    let enrolled = all(&r);
    let mut s = on_main();
    low(&mut s, "pool-a");
    limited(&mut s, "pool-a", NOW + HOUR);

    let event = pin(&mut s, &r, &enrolled, "pool-a", NOW).expect("pins");
    assert_eq!((event.kind, event.account.as_str()), (Kind::Use, "pool-a"));
    assert!(event.wakes(), "a waiting tab decides again");
    assert!(s.accounts["pool-a"].walls.is_empty());
    assert_eq!(
        (
            s.active.as_deref(),
            s.previous.as_deref(),
            s.proven,
            s.pinned
        ),
        (Some("pool-a"), Some("main"), true, true)
    );
    assert_eq!(
        rotation(&mut s.clone(), Some("pool-a"), "pool-a", "main", NOW),
        Err(Lost::Pinned)
    );
    let row = unpin(&mut s, "use --auto", NOW).expect("a pin held");
    assert_eq!(
        (row.kind, row.account.as_str(), row.why.as_str()),
        (Kind::Unpin, "pool-a", "use --auto")
    );
    assert_eq!(unpin(&mut s, "use --auto", NOW), None, "no pin, no row");

    // Found signed out, the pinned account's pin ends.
    let mut s = on_main();
    pin(&mut s, &r, &enrolled, "main", NOW).expect("pins");
    let events = note_auth(&mut s, "main", Auth::SignedOut, NOW);
    assert_eq!(
        events.iter().map(|e| e.kind).collect::<Vec<_>>(),
        [Kind::Auth, Kind::Unpin]
    );
    assert!(!s.pinned);
    // A wall NOTED again on a pinned account that already holds it (a state
    // an older build left): no second `wall` row, and the pin ends all the
    // same.
    let mut odd = on_main();
    limited(&mut odd, "main", NOW + HOUR);
    odd.pinned = true;
    let again = note_wall(
        &mut odd,
        "main",
        "five_hour",
        Some(NOW + HOUR - RESET_GRACE_S),
        NOW,
        NOW,
    );
    assert_eq!(
        again.iter().map(|e| e.kind).collect::<Vec<_>>(),
        [Kind::Unpin]
    );
    assert!(!odd.pinned);

    // Refused by name, and nothing changes: not listed, not enrolled,
    // signed out.
    let mut s = on_main();
    note_auth(&mut s, "work-2", Auth::SignedOut, NOW);
    let before = s.clone();
    let mut here = enrolled.clone();
    here.remove("pool-a");
    for label in ["nobody", "pool-a", "work-2"] {
        assert!(pin(&mut s, &r, &here, label, NOW).is_err(), "{label}");
    }
    assert_eq!(s, before);
}

/// THE OWNER LIFTED THIS WALL (`Advice::Resume`, between rules 2 and 3). A
/// session stands at its account's limit; the owner says `accounts use` for
/// that account, which clears its walls and pins it; the session's row is
/// older than the clearing, so it continues where it is. The model binds that
/// at every `Use…`; what a notice that outlasts the owner's word is advised is
/// this test's: a row of the clearing's own second or after, a wall met
/// again, `use --auto`, the owner's later word, a canary's hold, a login known
/// signed out, and a clearing stamped ahead of the clock.
#[test]
fn the_owners_clearing_resumes_a_session_at_the_wall_it_lifted() {
    let r = three();
    let enrolled = all(&r);
    let written = NOW - HOUR;
    let used = NOW - MIN;
    let until = Some(NOW + 3 * HOUR);
    let ask =
        |s: &State, point: &Point| advise(&r, s, &enrolled, &me("main"), point, Mode::Wall, NOW);
    let rotate = Advice::Move {
        to: "work-2".into(),
        ahead: false,
        rotates: true,
        takes_canary: false,
    };

    let mut s = on_main();
    assert_eq!(
        note_wall(&mut s, "main", "five_hour", until, written, written).len(),
        1
    );
    pin(&mut s, &r, &enrolled, "main", used).expect("pins");
    let resume = Advice::Resume { cleared_at: used };
    assert_eq!(ask(&s, &wall_at(written)), resume);
    // A row of the clearing's own second, or after it, is a wall met since.
    for met in [used, used + 1, NOW] {
        assert_eq!(ask(&s, &wall_at(met)), rotate, "{met}");
    }

    // THE ACCOUNT ANSWERS WITH A NEW WALL: recorded, the pin ends, and the
    // rotation commits — and a second session still standing at the OLD row
    // is not resumed onto an account a wall stands on again.
    let met = NOW - 30;
    let mut again = s.clone();
    let rows = note_wall(&mut again, "main", "five_hour", until, met, NOW);
    assert_eq!(
        rows.iter().map(|e| e.kind).collect::<Vec<_>>(),
        [Kind::Wall, Kind::Unpin]
    );
    assert_eq!(ask(&again, &wall_at(met)), rotate);
    let who = mover("main");
    let committed = begin_rotation(
        &mut again.clone(),
        &r,
        &enrolled,
        Some("main"),
        who,
        "work-2",
        NOW,
    );
    assert!(committed.is_ok(), "{committed:?}");
    assert_eq!(ask(&again, &wall_at(written)), rotate);

    // THE CLEARING IS WHAT IS ASKED, not the pin: `use --auto` after it
    // leaves the wall lifted.
    let mut auto = s.clone();
    unpin(&mut auto, "use --auto", NOW).expect("a pin held");
    assert_eq!(ask(&auto, &wall_at(written)), resume);

    // RULE 2 COMES FIRST. The owner's later word for another account is
    // followed…
    let mut moved_on = s.clone();
    pin(&mut moved_on, &r, &enrolled, "work-2", NOW - 30).expect("pins");
    assert_eq!(
        ask(&moved_on, &wall_at(written)),
        Advice::Move {
            to: "work-2".into(),
            ahead: false,
            rotates: false,
            takes_canary: false
        }
    );
    // …and an account that has not answered yet keeps its canary's hold.
    let unanswered = State {
        active: Some("work-2".into()),
        proven: false,
        pinned: false,
        canary: Some(Canary {
            sid: "s-two".into(),
            at: NOW - MIN,
            took: false,
        }),
        ..s.clone()
    };
    assert_eq!(
        ask(&unanswered, &wall_at(written)),
        Advice::Stay(StayWhy::Canary)
    );

    // A login known signed out is not continued on.
    let mut out = s.clone();
    note_auth(&mut out, "main", Auth::SignedOut, NOW);
    assert_eq!(ask(&out, &wall_at(written)), rotate);

    // A CLEARING STAMPED AHEAD OF THE CLOCK is read as made now, and its
    // key with it (stated on `advise`: such a key moves with the clock).
    let mut ahead = s.clone();
    ahead.accounts.get_mut("main").expect("cleared").cleared_at = Some(NOW + DAY);
    assert_eq!(
        ask(&ahead, &wall_at(written)),
        Advice::Resume { cleared_at: NOW }
    );
}

/// THE WAIT ENDS AT WHATEVER COMES FIRST. A session held at its wall — by a
/// canary's hold, or by the machine's rotation bound — waits for the end of
/// what holds it, or for the roster's next opening where that is sooner.
///
/// Held for the hour beside an active account whose own wall ended twenty
/// minutes in, the session followed it forty minutes late. (That case, through
/// the real decider at every hold, is the model walk's; here are the edges.)
#[test]
fn a_held_sessions_wait_ends_at_the_rosters_next_opening_when_that_comes_first() {
    let r = three();
    let enrolled = all(&r);
    let opening = |s: &State| next_opening(&r, s, &enrolled, NOW);

    // THE NEXT OPENING is an instant AHEAD: an account usable now has none.
    let mut s = on_main();
    fresh(&mut s, "main");
    assert_eq!(opening(&s), None, "nothing is due to open");
    limited(&mut s, "pool-a", NOW + 20 * MIN);
    limited(&mut s, "work-2", NOW + 2 * HOUR);
    assert_eq!(opening(&s), Some(NOW + 20 * MIN));
    // NEGATIVE CONTROL: the report's question counts the account usable now.
    assert_eq!(next_usable(&r, &s, &enrolled, NOW), Some(NOW));

    // THE CANARY'S HOLD: its end, cut at an opening before it.
    let held = State {
        active: Some("work-2".into()),
        proven: false,
        canary: Some(Canary {
            sid: "s-two".into(),
            at: NOW - 4 * MIN,
            took: false,
        }),
        ..state()
    };
    let end = NOW - 4 * MIN + CANARY_HOLD_S;
    assert_eq!(held.canary_until(NOW), Some(end));
    let canary = |opening: Option<i64>| hold_until(StayWhy::Canary, &held, opening, NOW);
    assert_eq!(canary(None), Some(end));
    assert_eq!(canary(Some(NOW + 2 * MIN)), Some(NOW + 2 * MIN));
    assert_eq!(canary(Some(end + MIN)), Some(end), "an opening after it");
    for not_ahead in [NOW, NOW - MIN] {
        assert_eq!(canary(Some(not_ahead)), Some(end), "{not_ahead}");
    }
    // NEGATIVE CONTROLS: no hold stands — lapsed, or the account answered.
    let lapsed = State {
        canary: Some(Canary {
            sid: "s-two".into(),
            at: NOW - CANARY_HOLD_S,
            took: false,
        }),
        ..held.clone()
    };
    let answered = State {
        proven: true,
        ..held.clone()
    };
    for none in [&lapsed, &answered] {
        assert_eq!(none.canary_until(NOW), None);
        assert_eq!(
            hold_until(StayWhy::Canary, none, Some(NOW + MIN), NOW),
            None
        );
    }

    // THE ROTATION BOUND: the hour's end, cut the same way.
    let bound = StayWhy::Breaker {
        until: NOW + 50 * MIN,
    };
    assert_eq!(hold_until(bound, &held, None, NOW), Some(NOW + 50 * MIN));
    assert_eq!(
        hold_until(bound, &held, Some(NOW + 20 * MIN), NOW),
        Some(NOW + 20 * MIN)
    );
    assert_eq!(
        hold_until(bound, &held, Some(NOW + HOUR), NOW),
        Some(NOW + 50 * MIN)
    );
    // Nothing else holds a session.
    for why in [
        StayWhy::NoCandidate,
        StayWhy::NothingDue,
        StayWhy::Pinned,
        StayWhy::NoFresh,
    ] {
        assert_eq!(
            hold_until(why, &held, Some(NOW + MIN), NOW),
            None,
            "{why:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 5. Transactions
// ---------------------------------------------------------------------------

/// A ROTATION is the session's alone: its row names it, the account it chose
/// is active and not proven, and it counts toward the machine's bound.
/// Another session cannot rotate past that canary by asking for another
/// account. The account is proven by its own first served response, once.
/// (The race and who follows are the model walk's.)
#[test]
fn a_rotation_is_one_sessions_and_is_proven_by_its_accounts_first_response() {
    let mut s = on_main();
    limited(&mut s, "main", NOW + 3 * HOUR);
    let won = rotation(&mut s, Some("main"), "main", "work-2", NOW).expect("rotates");
    assert_eq!(
        (
            won.kind,
            won.account.as_str(),
            won.to.as_deref(),
            won.sid.as_deref(),
            won.t
        ),
        (Kind::Rotate, "main", Some("work-2"), Some("s-one"), NOW)
    );
    assert_eq!(
        (s.active.as_deref(), s.previous.as_deref(), s.proven),
        (Some("work-2"), Some("main"), false)
    );
    assert_eq!(s.rotations, [NOW], "it counts toward the machine's bound");
    assert_eq!(s.canary_since(), Some(NOW));

    let before = s.clone();
    assert_eq!(
        rotation(&mut s, Some("work-2"), "main", "pool-a", NOW),
        Err(Lost::Held {
            active: "work-2".into()
        })
    );
    assert_eq!(s, before);

    assert_eq!(
        prove(&mut s, "pool-a", NOW + 30),
        None,
        "another account's response proves nothing"
    );
    let proven = prove(&mut s, "work-2", NOW + 30).expect("the first served response");
    assert_eq!(
        (proven.kind, proven.account.as_str()),
        (Kind::Proven, "work-2")
    );
    assert!(proven.wakes());
    assert!(s.proven);
    assert_eq!(
        s.accounts["work-2"].auth,
        Auth::Ok,
        "a served response says the login works"
    );
    assert_eq!(prove(&mut s, "work-2", NOW + 60), None, "proven once");
    assert_eq!(s.canary_since(), None);
}

#[test]
fn the_compare_and_set_refuses_by_name_and_changes_nothing() {
    let r = three();
    let enrolled = all(&r);
    let mut s = on_main();
    limited(&mut s, "main", NOW + HOUR);
    limited(&mut s, "pool-a", NOW + HOUR);
    let before = s.clone();
    let mut go = |expect: Option<&str>, me: &str, to: &str| {
        let out = begin_rotation(&mut s, &r, &enrolled, expect, mover(me), to, NOW);
        assert_eq!(s, before, "a refusal writes nothing");
        out
    };
    assert_eq!(
        go(None, "main", "work-2"),
        Err(Lost::Raced {
            active: Some("main".into())
        })
    );
    for to in ["main", "pool-a", "nobody"] {
        assert_eq!(
            go(Some("main"), "main", to),
            Err(Lost::NotACandidate),
            "{to}"
        );
    }

    // A MOVER WITH NO SESSION ID could be no canary: nobody could answer
    // for the account it chose, take its place by name or be forgotten.
    for sid in ["", " \n\t"] {
        let nobody = Mover {
            sid,
            account: "main",
        };
        let out = begin_rotation(&mut s, &r, &enrolled, Some("main"), nobody, "work-2", NOW);
        assert_eq!(out, Err(Lost::NoSession), "{sid:?}");
        assert_eq!(s, before, "a refusal writes nothing");
    }
    // NEGATIVE CONTROL: the same call from a session with an id commits.
    assert!(rotation(&mut s.clone(), Some("main"), "main", "work-2", NOW).is_ok());
    // …and no id is no session to forget: a canary's record that names
    // none (a file written by hand) is not dropped for it.
    let mut nameless = State {
        active: Some("work-2".into()),
        canary: Some(Canary {
            sid: String::new(),
            at: NOW,
            took: false,
        }),
        ..state()
    };
    let unnamed = nameless.clone();
    assert!(!forget_session(&mut nameless, ""));
    assert_eq!(nameless, unnamed);
    assert!(nameless.canary_holds(NOW), "its hold stands");
    // …and no id is no session to speak as that canary either: a refused
    // relaunch said with none is no step, though the record's own id is the
    // same nothing.
    let not_moved = Strike::NotMoved("wait:changed".into());
    assert!(strike(&mut nameless, "work-2", "", &not_moved, NOW).is_empty());
    assert_eq!(nameless, unnamed, "nobody's word undoes nothing");

    // A proven active account that can be run on is not rotated past by a
    // session on another account (one that cannot be, is: the model's).
    let mut s = on_main();
    assert_eq!(
        rotation(&mut s, Some("main"), "work-2", "pool-a", NOW),
        Err(Lost::Held {
            active: "main".into()
        })
    );
}

/// A STRUCK CANDIDATE puts `active` back, by the word it answered with: its
/// own wall (limited), a login wall (signed out, which the model has not), a
/// move that failed with no word (the no-reset ladder). A refused relaunch
/// says nothing about an account that is not the canary's; a strike of an
/// account that is not the unproven active one marks it and moves nothing.
#[test]
fn a_struck_candidate_puts_active_back_and_nobody_else_was_moved() {
    let r = three();
    let start = || {
        let mut s = on_main();
        limited(&mut s, "main", NOW + 3 * HOUR);
        rotation(&mut s, Some("main"), "main", "work-2", NOW).expect("rotates");
        s
    };
    let kinds = |events: &[Event]| events.iter().map(|e| e.kind).collect::<Vec<_>>();

    let mut s = start();
    let own_wall = Strike::Wall {
        window: "five_hour".into(),
        until: Some(NOW + 2 * HOUR),
    };
    let events = strike(&mut s, "work-2", "s-one", &own_wall, NOW + 20);
    assert_eq!(kinds(&events), [Kind::Struck, Kind::Wall]);
    assert_eq!(
        (events[0].account.as_str(), events[0].to.as_deref()),
        ("work-2", Some("main"))
    );
    assert!(events[0].wakes());
    assert_eq!(
        (s.active.as_deref(), s.previous.as_deref(), s.proven),
        (Some("main"), None, true)
    );
    assert_eq!(
        cond(&r, &s, "work-2", NOW + 20),
        Condition::Limited {
            until: NOW + 2 * HOUR + 150
        }
    );

    let mut s = start();
    let events = strike(&mut s, "work-2", "s-one", &Strike::SignedOut, NOW + 20);
    assert_eq!(kinds(&events), [Kind::Struck, Kind::Auth]);
    assert_eq!(s.active.as_deref(), Some("main"));
    assert_eq!(cond(&r, &s, "work-2", NOW + DAY), Condition::SignedOut);

    let mut s = start();
    strike(
        &mut s,
        "work-2",
        "s-one",
        &Strike::Failed("no-resume".into()),
        NOW + 20,
    );
    assert_eq!(s.active.as_deref(), Some("main"));
    assert_eq!(
        cond(&r, &s, "work-2", NOW + 20),
        Condition::Limited {
            until: NOW + 20 + 30 * MIN
        }
    );
    assert!(s.accounts["work-2"].walls.contains_key(STRUCK_WINDOW));
    assert_eq!(
        cond(&r, &s, "work-2", NOW + 20 + 30 * MIN),
        Condition::Usable
    );

    let mut s = start();
    let not_moved = Strike::NotMoved("wait:changed".into());
    assert!(strike(&mut s, "pool-a", "s-one", &not_moved, NOW).is_empty());

    // NEGATIVE CONTROL: an account that is not the unproven active one is
    // marked, and `active` does not move.
    let mut s = start();
    prove(&mut s, "work-2", NOW + 10).expect("proven");
    strike(&mut s, "work-2", "s-one", &Strike::SignedOut, NOW + 20);
    assert_eq!((s.active.as_deref(), s.proven), (Some("work-2"), true));
    let mut s = start();
    strike(&mut s, "pool-a", "s-one", &Strike::SignedOut, NOW + 20);
    assert_eq!((s.active.as_deref(), s.proven), (Some("work-2"), false));
}

/// THE CANARY'S HOLD LASTS TEN MINUTES, to the second, and is over however
/// long after (the model's `Lapse` has no clock). The take is a
/// compare-and-set on the canary's record that moves nothing but the record,
/// and the undo with it. A hold stamped ahead of the clock is no hold; a
/// proven account has no place to take.
#[test]
fn the_canarys_hold_lasts_ten_minutes_then_the_next_session_takes_its_place() {
    let r = three();
    let enrolled = all(&r);
    let mut s = on_main();
    limited(&mut s, "main", NOW + DAY);
    rotation(&mut s, Some("main"), "main", "work-2", NOW).expect("rotates");
    let first = s.canary.clone().expect("the rotation's canary");
    let at_wall =
        |s: &State, now: i64| advise(&r, s, &enrolled, &me("main"), &wall(), Mode::Early, now);
    let take = Advice::Move {
        to: "work-2".into(),
        ahead: false,
        rotates: false,
        takes_canary: true,
    };

    let last = NOW + CANARY_HOLD_S - 1;
    assert_eq!(at_wall(&s, last), Advice::Stay(StayWhy::Canary));
    assert_eq!(
        take_canary(&mut s.clone(), Some(&first), "s-two", last),
        None
    );
    let lapsed = NOW + CANARY_HOLD_S;
    assert_eq!(at_wall(&s, lapsed), take);
    assert_eq!(at_wall(&s, NOW + 30 * DAY), take, "nor a month on");

    let before = s.clone();
    let took = take_canary(&mut s, Some(&first), "s-two", lapsed).expect("takes the place");
    assert_eq!(
        (
            took.kind,
            took.account.as_str(),
            took.to.as_deref(),
            took.sid.as_deref()
        ),
        (Kind::Rotate, "work-2", Some("work-2"), Some("s-two"))
    );
    let second = Canary {
        sid: "s-two".into(),
        at: lapsed,
        took: true,
    };
    assert_eq!(before.undo.len(), 1);
    assert_eq!(
        s,
        State {
            canary: Some(second.clone()),
            undo: Vec::new(),
            ..before.clone()
        }
    );

    let ahead = State {
        canary: Some(Canary {
            at: lapsed + DAY,
            ..second
        }),
        ..s.clone()
    };
    assert_eq!(at_wall(&ahead, lapsed), take);

    let mut served = s.clone();
    prove(&mut served, "work-2", lapsed + 5).expect("proven");
    assert_eq!(
        take_canary(&mut served, None, "s-three", lapsed + DAY),
        None,
        "a proven account has no place to take"
    );
}

/// A RELAUNCH THE HOST REFUSES TAKES THE ROTATION BACK EXACTLY — past an
/// account that had itself not answered too: that account's `previous`, proof
/// and canary record come back, so a later strike of it does not go back to
/// nothing. (Whose word it is, and the undo of one rotation, are the model
/// walk's; its projection carries no undo.)
#[test]
fn a_refused_relaunch_takes_the_rotation_back_exactly() {
    let r = three();
    let enrolled = all(&r);
    let not_moved = Strike::NotMoved("wait:changed".into());
    let rotate = |s: &mut State, sid, from, to, now| {
        let read = s.active.clone();
        let who = Mover { sid, account: from };
        begin_rotation(s, &r, &enrolled, read.as_deref(), who, to, now).expect("rotates")
    };
    let mut start = on_main();
    limited(&mut start, "main", NOW + DAY);
    let mut rotated = start.clone();
    rotate(&mut rotated, "s-one", "main", "work-2", NOW);
    assert_eq!(rotated.undo, [Undo::default()], "what it wrote over");

    // PAST AN ACCOUNT THAT HAD NOT ANSWERED EITHER. A wall is recorded on
    // work-2 with no strike, `s-two` rotates past it to pool-a, and ITS
    // relaunch is refused: work-2 is `active` again exactly as it was left —
    // the account a strike of it goes back to, that account's proof, and
    // the record of the canary still on it.
    let mut s = rotated.clone();
    let seen = NOW + MIN;
    assert_eq!(
        note_wall(
            &mut s,
            "work-2",
            "five_hour",
            Some(NOW + 2 * HOUR),
            seen,
            seen
        )
        .len(),
        1
    );
    let left = s.clone();
    rotate(&mut s, "s-two", "main", "pool-a", seen + 30);
    assert_eq!(
        (s.previous.as_deref(), s.previous_proven, s.rotations.len()),
        (Some("work-2"), false, 2)
    );
    assert_eq!(
        s.undo.last(),
        Some(&Undo {
            previous: Some("main".into()),
            previous_proven: true,
            canary: rotated.canary.clone(),
        })
    );
    let second = s.clone();
    let events = strike(&mut s, "pool-a", "s-two", &not_moved, seen + 35);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].to.as_deref(), Some("work-2"));
    assert_eq!(s, left, "everything the rotation wrote is put back");
    assert!(s.canary_holds(seen + 35), "its hold was never given up");
    // The first rotation, refused in its turn, is taken back as exactly:
    // the state before either, with the wall that was seen since.
    assert_eq!(
        strike(&mut s, "work-2", "s-one", &not_moved, seen + 40).len(),
        1
    );
    let mut before_either = start.clone();
    note_wall(
        &mut before_either,
        "work-2",
        "five_hour",
        Some(NOW + 2 * HOUR),
        seen,
        seen,
    );
    assert_eq!(s, before_either);

    // THE CANARY OF THE ACCOUNT ROTATED PAST ENDS (its agent did) before
    // the later rotation is taken back: its record is not put back to hold
    // anyone, and the rotation it made is nobody's to undo.
    let mut s = second.clone();
    assert!(forget_session(&mut s, "s-one"));
    assert_eq!(s.undo.len(), 1, "its own rotation's undo went with it");
    strike(&mut s, "pool-a", "s-two", &not_moved, seen + 35);
    assert_eq!(
        s,
        State {
            canary: None,
            undo: Vec::new(),
            ..left.clone()
        }
    );
    assert!(!s.canary_holds(seen + 35));

    // WHAT IS NOT PUT BACK: a pin the rotation ended. It was left on an
    // account that cannot be run on, and is the owner's to give again.
    let here: BTreeSet<String> = ["main", "work-2"].map(str::to_string).into();
    let pinned = State {
        active: Some("pool-a".into()),
        proven: true,
        pinned: true,
        ..start.clone()
    };
    let mut s = pinned.clone();
    let who = mover("main");
    begin_rotation(&mut s, &r, &here, Some("pool-a"), who, "work-2", NOW).expect("rotates");
    assert!(!s.pinned);
    strike(&mut s, "work-2", "s-one", &not_moved, NOW + 5);
    assert_eq!(
        s,
        State {
            pinned: false,
            ..pinned.clone()
        }
    );

    // NOTHING IS LEFT TO TAKE BACK once the account has answered, the owner
    // has spoken, the canary's agent has ended, or another session took its
    // place.
    let mut taken = rotated.clone();
    let then = NOW + CANARY_HOLD_S + 100;
    take_canary(&mut taken, rotated.canary.as_ref(), "s-two", then).expect("takes the place");
    let mut served = rotated.clone();
    prove(&mut served, "work-2", NOW + 5).expect("proven");
    let mut owned = rotated.clone();
    pin(&mut owned, &r, &enrolled, "pool-a", NOW + 5).expect("pins");
    let mut ended = rotated.clone();
    assert!(forget_session(&mut ended, "s-one"));
    for settled in [&served, &owned, &ended, &taken] {
        assert!(settled.undo.is_empty());
        let mut late = settled.clone();
        assert!(strike(&mut late, "work-2", "s-one", &not_moved, NOW + 9).is_empty());
        assert_eq!(&late, settled);
    }
}

/// A CANDIDATE THAT ANSWERS BADLY, CHOSEN PAST AN ACCOUNT THAT HAD NOT
/// ANSWERED EITHER, LEAVES THAT ACCOUNT AS THE ROTATION FOUND IT. `s-one`
/// rotates main -> work-2 and hits work-2's wall before any response (a wall
/// recorded with no strike); `s-two` rotates past it to pool-a, which
/// answers with a wall of its own. `active` is work-2 again WITH ITS OWN
/// `previous` (main, proven) AND ITS OWN CANARY (`s-one`'s record), so a
/// later strike of work-2 goes back to main, proven.
///
/// Were the strike to drop what the rotation wrote over, the second strike
/// would go back to NOTHING — no active account, with a proven one
/// standing — and the session still alone on work-2 would hold nobody.
#[test]
fn a_candidate_struck_past_an_unanswered_account_leaves_that_account_as_it_was() {
    let r = three();
    let enrolled = all(&r);
    let rotate = |s: &mut State, sid, from, to, now| {
        let read = s.active.clone();
        let who = Mover { sid, account: from };
        begin_rotation(s, &r, &enrolled, read.as_deref(), who, to, now).expect("rotates")
    };
    let mut start = on_main();
    limited(&mut start, "main", NOW + DAY);
    let mut s = start.clone();
    rotate(&mut s, "s-one", "main", "work-2", NOW);
    let s_ones = s.canary.clone();
    assert_eq!(s_ones.as_ref().map(|c| c.sid.as_str()), Some("s-one"));
    let seen = NOW + MIN;
    let until = Some(NOW + 2 * HOUR);
    assert_eq!(
        note_wall(&mut s, "work-2", "five_hour", until, seen, seen).len(),
        1
    );
    let left = s.clone();
    rotate(&mut s, "s-two", "main", "pool-a", seen + 30);
    let second = s.clone();
    assert_eq!(
        (s.previous.as_deref(), s.previous_proven, s.undo.len()),
        (Some("work-2"), false, 2)
    );

    // Every bad answer, from the session that moved and from any other on
    // the account: the same account gone back to, the same way.
    let words = [
        Strike::Wall {
            window: "seven_day".into(),
            until: Some(NOW + 3 * DAY),
        },
        Strike::SignedOut,
        Strike::Failed("no-resume".into()),
    ];
    for (word, sid) in words.iter().zip(["s-two", "s-two", "s-three"]) {
        let mut s = second.clone();
        let at = seen + 40;
        let events = strike(&mut s, "pool-a", sid, word, at);
        assert_eq!(events[0].kind, Kind::Struck, "{word:?}");
        assert_eq!(events[0].to.as_deref(), Some("work-2"), "{word:?}");
        assert_eq!(
            (
                s.active.as_deref(),
                s.proven,
                s.previous.as_deref(),
                s.previous_proven,
                &s.canary
            ),
            (Some("work-2"), false, Some("main"), true, &s_ones),
            "{word:?}"
        );
        assert_eq!(s.undo, left.undo, "its own rotation's undo: {word:?}");
        assert!(
            s.canary_holds(at),
            "the session on it holds for the rest of ITS ten minutes: {word:?}"
        );
        assert!(!s.canary_holds(NOW + CANARY_HOLD_S), "…and no longer");
        // THE ROTATION STAYS COUNTED, and the word stays on record.
        assert_eq!(s.rotations.len(), 2, "{word:?}");
        assert!(
            !cond(&r, &s, "pool-a", at).runs(),
            "{word:?}: {:?}",
            cond(&r, &s, "pool-a", at)
        );
        // Nothing else of the state before the second rotation moved.
        assert_eq!(
            State {
                accounts: left.accounts.clone(),
                rotations: left.rotations.clone(),
                ..s.clone()
            },
            left,
            "{word:?}"
        );

        // THE RELAUNCH OF THE SESSION ON work-2 REFUSED, heard now, takes
        // ITS OWN rotation back — and its own time out of the bound, which
        // is no longer the newest: the struck rotation's stays counted.
        let mut refused = s.clone();
        let not_moved = Strike::NotMoved("wait:changed".into());
        let events = strike(&mut refused, "work-2", "s-one", &not_moved, at + 5);
        assert_eq!(events[0].to.as_deref(), Some("main"), "{word:?}");
        assert_eq!(
            (
                refused.active.as_deref(),
                refused.proven,
                refused.previous.as_deref(),
                &refused.canary,
                refused.undo.len()
            ),
            (Some("main"), true, None, &None, 0),
            "{word:?}"
        );
        assert_eq!(s.rotations, [NOW, seen + 30], "{word:?}");
        assert_eq!(refused.rotations, [seen + 30], "{word:?}");

        // A LATER STRIKE OF work-2 GOES BACK TO main, PROVEN.
        let events = strike(&mut s, "work-2", "s-one", &Strike::SignedOut, at + 5);
        assert_eq!(events[0].to.as_deref(), Some("main"), "{word:?}");
        assert_eq!(
            (
                s.active.as_deref(),
                s.proven,
                s.previous.as_deref(),
                &s.canary,
                s.undo.len()
            ),
            (Some("main"), true, None, &None, 0),
            "{word:?}"
        );
    }

    // NEGATIVE CONTROL: the same strike with nothing kept of what the
    // rotation wrote over (a state written before the undo was) goes back to
    // work-2 with no `previous` and no canary — and the strike after it to
    // no account at all.
    let mut older = State {
        undo: Vec::new(),
        ..second.clone()
    };
    strike(&mut older, "pool-a", "s-two", &words[0], seen + 40);
    assert_eq!(
        (
            older.active.as_deref(),
            older.previous.as_deref(),
            older.previous_proven,
            &older.canary
        ),
        (Some("work-2"), None, false, &None)
    );
    strike(&mut older, "work-2", "s-one", &Strike::SignedOut, seen + 45);
    assert_eq!((older.active.as_deref(), older.proven), (None, false));
}

/// AT A WALL `me` IS LIMITED, WHETHER OR NOT ITS WALL IS ON RECORD YET. The
/// host records the wall in the same transaction as the move, and `advise`
/// gives the same answer asked before that or after.
///
/// Read as usable before its wall is recorded, `me` would turn "every other
/// account limited" into `Stay(NoCandidate)` instead of the wait.
#[test]
fn at_a_wall_the_session_is_limited_whether_or_not_its_wall_is_recorded_yet() {
    let r = three();
    let enrolled = all(&r);
    let at_wall = |s: &State| advise(&r, s, &enrolled, &me("main"), &wall(), Mode::Early, NOW);
    let recorded = |s: &State| {
        let mut out = s.clone();
        let rows = note_wall(
            &mut out,
            "main",
            "five_hour",
            Some(NOW + 4 * HOUR),
            NOW,
            NOW,
        );
        assert_eq!(rows.first().map(|e| e.kind), Some(Kind::Wall));
        out
    };

    // Every other account is limited: the wait is for the first of THEM,
    // not for the session's own account "usable now".
    let mut s = on_main();
    fresh(&mut s, "main");
    limited(&mut s, "work-2", NOW + 2 * HOUR);
    limited(&mut s, "pool-a", NOW + 3 * HOUR);
    let wait = Advice::Wait {
        until: NOW + 2 * HOUR,
    };
    assert_eq!(at_wall(&s), wait);
    assert_eq!(at_wall(&recorded(&s)), wait, "and the same once recorded");
    // An account nobody has observed is no more usable at its own wall.
    let mut unseen = s.clone();
    unseen.accounts.remove("main");
    assert_eq!(at_wall(&unseen), wait);
    // Its own wall's end is known only once it is recorded. Until then a
    // sample that shows it low says when that window ends…
    let mut soon = s.clone();
    let read = Sample {
        session_reset: Some(NOW + 10 * MIN),
        at: NOW,
        ..sample(95, 20)
    };
    assert_eq!(note_sample(&mut soon, "main", read, NOW).len(), 1);
    assert_eq!(
        at_wall(&soon),
        Advice::Wait {
            until: NOW + 10 * MIN
        }
    );
    // …and with no other account to wait for there is no instant at all:
    // the wall is waited out as it would be with no roster.
    let mut alone = on_main();
    fresh(&mut alone, "main");
    for label in ["work-2", "pool-a"] {
        note_auth(&mut alone, label, Auth::SignedOut, NOW);
    }
    assert_eq!(at_wall(&alone), Advice::Stay(StayWhy::NoCandidate));
    assert_eq!(
        at_wall(&recorded(&alone)),
        Advice::Wait {
            until: NOW + 4 * HOUR + RESET_GRACE_S
        }
    );
    // NEGATIVE CONTROL: the report asks for no session, and to it a fresh
    // account is usable now.
    assert_eq!(next_usable(&r, &s, &enrolled, NOW), Some(NOW));
}

/// THE LOST LOGIN SIGNED BACK IN ([`Point::LostLogin`]): a session moved
/// from `main`'s wall onto `work-2` finds `work-2`'s login lost (met at
/// `seen`); the host strikes it signed out, and with every other account
/// limited the session waits for the first of them. Once the owner signs
/// `work-2` in again (`accounts login`: its word `ok`, written after `seen`)
/// the same point is advised the session's OWN account, as a follow — at
/// both of the advice's arms. NEGATIVE CONTROLS: still signed out it is the
/// wait; so it is with the strike not on record and nothing said of the
/// login (`unseen` is not a login that came back), and with the strike not
/// on record and the login's `ok` from before the lost login (its
/// enrolment's check) or of its very second — while the same state with an
/// `ok` written after it is the login back; at a usage wall the session's
/// own account is never advised, whatever its login says; and an account
/// the owner pinned since is followed instead (rule 2 comes first).
///
/// THE CANARY'S HOLD: another session rotates onto the login signed back
/// in, and the lost login is then that canary's to wait behind
/// ([`StayWhy::Canary`]) — NEGATIVE CONTROLS: the account proven, it is
/// the plain follow; the hold lapsed, the move takes the canary's place,
/// and that commits.
#[test]
fn a_lost_login_signed_back_in_is_where_the_session_is_relaunched() {
    let r = three();
    let enrolled = all(&r);
    let at_when = |s: &State, point: &Point, now: i64| {
        let asked = advise(&r, s, &enrolled, &me("work-2"), point, Mode::Early, now);
        let past = advise_past_probe(&r, s, &enrolled, &me("work-2"), point, Mode::Early, now);
        assert_eq!(asked, past, "both arms");
        asked
    };
    let at = |s: &State, point: &Point| at_when(s, point, NOW + 60);
    let lost = Point::LostLogin { seen: NOW + 10 };
    let follow = |to: &str, takes_canary: bool| Advice::Move {
        to: to.into(),
        ahead: false,
        rotates: false,
        takes_canary,
    };
    let mut moved = on_main();
    limited(&mut moved, "main", NOW + 3 * DAY);
    limited(&mut moved, "pool-a", NOW + 2 * DAY);
    rotation(&mut moved, Some("main"), "main", "work-2", NOW).expect("rotates");
    let wait = Advice::Wait {
        until: NOW + 2 * DAY,
    };
    // The strike not on record: nothing says the login works.
    assert_eq!(at(&moved, &lost), wait);
    // …nor does the word it had before it was lost (R1): its enrolment's
    // check, or one of the very second the lost login was met.
    for before in [NOW - DAY, NOW + 10] {
        let mut old = moved.clone();
        note_auth(&mut old, "work-2", Auth::Ok, before);
        assert_eq!(at(&old, &lost), wait, "an ok of {before}");
    }
    // NEGATIVE CONTROL: the same state with the sign-in after it is the
    // login back — behind this rotation's own canary, whose hold stands.
    let mut since = moved.clone();
    note_auth(&mut since, "work-2", Auth::Ok, NOW + 50);
    assert_eq!(at(&since, &lost), Advice::Stay(StayWhy::Canary));

    let mut struck = moved.clone();
    strike(&mut struck, "work-2", "s-one", &Strike::SignedOut, NOW + 30);
    assert_eq!(struck.active.as_deref(), Some("main"), "put back");
    // NEGATIVE CONTROL: still signed out.
    assert_eq!(at(&struck, &lost), wait);

    let mut back = struck.clone();
    note_auth(&mut back, "work-2", Auth::Ok, NOW + 50);
    assert_eq!(at(&back, &lost), follow("work-2", false));
    // NEGATIVE CONTROL: a usage wall says the account is limited.
    assert_eq!(at(&back, &wall()), wait);
    // NEGATIVE CONTROL: the owner pinned another account since.
    let mut pinned = back.clone();
    pin(&mut pinned, &r, &enrolled, "main", NOW + 55).expect("pinned");
    assert_eq!(at(&pinned, &lost), follow("main", false));

    // Another session at `main`'s wall rotates onto the login signed back
    // in first (R2): this one waits behind its canary.
    let mut canary = back.clone();
    let other = Mover {
        sid: "s-two",
        account: "main",
    };
    begin_rotation(
        &mut canary,
        &r,
        &enrolled,
        Some("main"),
        other,
        "work-2",
        NOW + 55,
    )
    .expect("rotates");
    assert_eq!(at(&canary, &lost), Advice::Stay(StayWhy::Canary));
    // NEGATIVE CONTROL: proven, the plain follow.
    let mut proven = canary.clone();
    prove(&mut proven, "work-2", NOW + 58);
    assert_eq!(at(&proven, &lost), follow("work-2", false));
    // NEGATIVE CONTROL: the hold lapsed, the canary's place is taken.
    let lapsed = NOW + 55 + CANARY_HOLD_S + 1;
    assert_eq!(at_when(&canary, &lost, lapsed), follow("work-2", true));
    let expect = canary.canary.clone();
    let mut taken = canary.clone();
    assert!(take_canary(&mut taken, expect.as_ref(), "s-one", lapsed).is_some());
    assert_eq!(taken.canary.map(|c| c.sid), Some("s-one".to_string()));
}

/// EVERY ROTATION THE ADVICE GIVES COMMITS, over every small state: three
/// accounts each never seen, fresh, low or limited; `active` nothing or any
/// of them — proven, pinned, or not answered yet with its canary's hold
/// standing, lapsed or on no record; the hour's rotations spent or not; each
/// session, at a wall recorded or not yet, and at a plain turn end.
///
/// At a wall the commit is on the state WITH THE WALL RECORDED (the host's
/// one transaction), and the advice is the same asked before it is recorded
/// or after. The pin never refuses an advised rotation: a wall on the pinned
/// account has ended it. And every commit, refused its relaunch, is taken
/// back to exactly the state it was made on (less a pin the rotation ended:
/// one left on an account that cannot be run on, which no transaction here
/// leaves).
#[test]
fn every_rotation_the_advice_gives_commits_once_the_wall_is_recorded() {
    let r = three();
    let enrolled = all(&r);
    let labels = ["main", "work-2", "pool-a"];
    let not_moved = Strike::NotMoved("wait:changed".into());
    let held = Some(CANARY_HOLD_S / 2);
    let lapsed = Some(CANARY_HOLD_S);
    // (proven, pinned, how long ago the canary moved) for an active account.
    let machines = [
        (true, false, None),
        (true, true, None),
        (false, false, None),
        (false, false, held),
        (false, false, lapsed),
    ];
    let (mut states, mut asked, mut rotations, mut ahead_of_a_record) = (0, 0, 0, 0);
    let (mut pins_ended, mut pinned_before, mut held_before) = (0, 0, 0);
    for conds in 0..64_usize {
        let mut base = state();
        for (k, label) in labels.iter().enumerate() {
            match (conds >> (2 * k)) & 3 {
                0 => {}
                1 => fresh(&mut base, label),
                2 => low(&mut base, label),
                _ => limited(&mut base, label, NOW + 3 * HOUR),
            }
        }
        for active in [None, Some("main"), Some("work-2"), Some("pool-a")] {
            let machines = if active.is_some() {
                &machines[..]
            } else {
                &machines[2..3]
            };
            for &(proven, pinned, moved) in machines {
                for spent in [false, true] {
                    // An account that has not answered was rotated to from
                    // another, which had: what an undo must not lose.
                    let previous = active
                        .filter(|_| !proven)
                        .and_then(|a| labels.iter().find(|l| **l != a));
                    let s = State {
                        active: active.map(str::to_string),
                        proven,
                        pinned,
                        previous: previous.map(|l| (*l).to_string()),
                        previous_proven: previous.is_some(),
                        canary: moved.map(|ago| Canary {
                            sid: "s-zero".into(),
                            at: NOW - ago,
                            took: false,
                        }),
                        rotations: vec![NOW - MIN; if spent { 2 * labels.len() } else { 0 }],
                        ..base.clone()
                    };
                    states += 1;
                    for mine in labels {
                        for point in [wall(), Point::TurnEnd] {
                            let at_wall = point != Point::TurnEnd;
                            let ask = |s: &State| {
                                advise(&r, s, &enrolled, &me(mine), &point, Mode::Early, NOW)
                            };
                            let advice = ask(&s);
                            asked += 1;
                            // The state the host commits on: the session's
                            // wall recorded, where it was not yet.
                            let mut recorded = s.clone();
                            let unrecorded = at_wall
                                && !matches!(cond(&r, &s, mine, NOW), Condition::Limited { .. });
                            if unrecorded {
                                let until = Some(NOW + 4 * HOUR);
                                note_wall(&mut recorded, mine, "five_hour", until, NOW, NOW);
                                assert_eq!(
                                    advice,
                                    ask(&recorded),
                                    "{mine} at its wall, recorded or not, on {s:?}"
                                );
                            }
                            let Advice::Move {
                                to, rotates: true, ..
                            } = &advice
                            else {
                                continue;
                            };
                            rotations += 1;
                            let who = Mover {
                                sid: "s-one",
                                account: mine,
                            };
                            let commit = |on: &mut State| {
                                begin_rotation(on, &r, &enrolled, active, who, to, NOW)
                            };
                            let mut committed = recorded.clone();
                            let won = commit(&mut committed);
                            assert!(won.is_ok(), "{won:?}: {mine} to {to} at {point:?} on {s:?}");
                            if unrecorded {
                                ahead_of_a_record += 1;
                                pins_ended += usize::from(s.pinned && !recorded.pinned);
                                // NEGATIVE CONTROL: before the wall is
                                // recorded the compare-and-set may refuse.
                                match commit(&mut s.clone()) {
                                    Err(Lost::Pinned) => pinned_before += 1,
                                    Err(Lost::Held { .. }) => held_before += 1,
                                    other => assert!(other.is_ok(), "{other:?} on {s:?}"),
                                }
                            }
                            // The relaunch refused: the state it was made
                            // on, less a pin the rotation ended (one left
                            // on an account that cannot be run on).
                            let undone = strike(&mut committed, to, "s-one", &not_moved, NOW);
                            assert_eq!(undone.len(), 1);
                            recorded.pinned = false;
                            assert_eq!(committed, recorded, "{mine} to {to} at {point:?}");
                        }
                    }
                }
            }
        }
    }
    assert_eq!((states, asked), (2048, 12_288));
    // Not vacuous: rotations were advised, at walls not yet recorded too,
    // some under a pin the wall then ended, and some the compare-and-set
    // refuses on the state as it was before the wall was recorded.
    assert!(rotations > 1000, "{rotations}");
    assert!(ahead_of_a_record > 300, "{ahead_of_a_record}");
    assert!(pins_ended > 0 && pinned_before == pins_ended);
    assert!(held_before > 0);
}

#[test]
fn the_machine_rotates_at_most_twice_the_roster_size_an_hour() {
    let r = three();
    let enrolled = all(&r);
    let bound = 2 * r.rows.len();
    let with = |times: Vec<i64>| State {
        rotations: times,
        ..on_main()
    };
    let times = |n: usize| {
        (0..n)
            .map(|i| NOW - 10 * i64::try_from(i).expect("fits"))
            .collect()
    };

    assert!(!rotations_spent(&with(times(bound - 1)), &r, NOW));
    assert!(rotations_spent(&with(times(bound)), &r, NOW));
    // The window, to the second: a rotation an hour old no longer counts.
    let mut edge: Vec<i64> = times(bound - 1);
    edge.push(NOW - 3599);
    assert!(rotations_spent(&with(edge.clone()), &r, NOW));
    *edge.last_mut().expect("one") = NOW - 3600;
    assert!(!rotations_spent(&with(edge), &r, NOW));
    // With no roster nothing rotates.
    assert!(rotations_spent(&state(), &Roster::default(), NOW));

    // Spent: the compare-and-set refuses, and the advice is to wait the
    // wall out — a wait, not a move.
    let mut s = with(times(bound));
    limited(&mut s, "main", NOW + HOUR);
    // The breaker NAMES ITS INSTANT: the oldest rotation it counts plus the
    // hour, when the bound has room again and a waiting tab decides again.
    let lapses = NOW - 10 * (i64::try_from(bound).expect("fits") - 1) + ROTATION_WINDOW_S;
    assert_eq!(breaker_until(&s, &r, NOW), Some(lapses));
    assert_eq!(
        advise(&r, &s, &enrolled, &me("main"), &wall(), Mode::Early, NOW),
        Advice::Stay(StayWhy::Breaker { until: lapses })
    );
    // One second before it the bound still holds; at it, the move is advised
    // and the compare-and-set takes it.
    assert_eq!(
        advise(
            &r,
            &s,
            &enrolled,
            &me("main"),
            &wall(),
            Mode::Early,
            lapses - 1
        ),
        Advice::Stay(StayWhy::Breaker { until: lapses })
    );
    assert_eq!(
        advise(&r, &s, &enrolled, &me("main"), &wall(), Mode::Early, lapses),
        Advice::Move {
            to: "work-2".into(),
            ahead: false,
            rotates: true,
            takes_canary: false
        }
    );
    assert_eq!(breaker_until(&s, &r, lapses), None, "not spent");
    assert_eq!(breaker_until(&with(times(bound - 1)), &r, NOW), None);
    assert_eq!(
        breaker_until(&with(times(bound)), &Roster::default(), NOW),
        None,
        "no roster: no clock gives it room"
    );
    // More counted than the bound (a row was removed): the instant enough
    // of them have lapsed, not the first.
    let over = with(vec![
        NOW - 30,
        NOW - 20,
        NOW - 10,
        NOW - 5,
        NOW - 4,
        NOW - 3,
        NOW - 2,
    ]);
    assert_eq!(
        breaker_until(&over, &r, NOW),
        Some(NOW - 20 + ROTATION_WINDOW_S)
    );
    let before = s.clone();
    assert_eq!(
        rotation(&mut s, Some("main"), "main", "work-2", NOW),
        Err(Lost::Spent)
    );
    assert_eq!(s, before);
    // NEGATIVE CONTROL: one under the bound rotates, the times older than
    // the window are dropped as it does, and the next one is refused.
    let mut old = times(bound - 1);
    old.push(NOW - 2 * HOUR);
    let mut s = with(old);
    limited(&mut s, "main", NOW + HOUR);
    assert!(rotation(&mut s, Some("main"), "main", "work-2", NOW).is_ok());
    assert_eq!(s.rotations.len(), bound);
    assert!(s.rotations.iter().all(|t| NOW - t < HOUR));
    assert!(rotations_spent(&s, &r, NOW));
}

#[test]
fn samples_offers_auth_and_sessions_are_recorded_newest_first() {
    let mut s = state();

    // The sample held becomes the one before it; an older one is dropped.
    let first = Sample {
        at: NOW - 600,
        ..sample(40, 10)
    };
    let events = note_sample(&mut s, "main", first.clone(), NOW);
    assert_eq!(events.len(), 1);
    assert_eq!((events[0].kind, events[0].t), (Kind::Sample, NOW - 600));
    let second = Sample {
        at: NOW,
        session_pct: None,
        ..sample(0, 12)
    };
    assert_eq!(note_sample(&mut s, "main", second.clone(), NOW).len(), 1);
    let held = &s.accounts["main"];
    assert_eq!(held.sample.as_ref().map(|x| x.at), Some(NOW));
    assert_eq!(held.before.as_ref().map(|x| x.at), Some(NOW - 600));
    assert_eq!(held.sample.as_ref().map(|x| x.by.as_str()), Some("0a0b"));
    let before = s.clone();
    assert!(
        note_sample(&mut s, "main", first, NOW).is_empty(),
        "older than the one held"
    );
    assert!(
        note_sample(&mut s, "main", second, NOW).is_empty(),
        "the same instant again"
    );
    // A panel with neither figure is no sample; a percent off the scale is
    // no figure.
    let empty = Sample {
        session_pct: None,
        week_pct: Some(PCT_MAX + 1),
        at: NOW + 60,
        ..sample(0, 0)
    };
    assert!(note_sample(&mut s, "main", empty, NOW).is_empty());
    assert_eq!(s, before);

    // The offer: the newer one, and no ledger row of its own.
    assert!(note_offer(
        &mut s,
        "main",
        None,
        2,
        Some(NOW + DAY),
        NOW,
        NOW
    ));
    assert!(
        !note_offer(&mut s, "main", None, 9, None, NOW - 1, NOW),
        "older"
    );
    assert_eq!(
        s.accounts["main"]
            .offer
            .as_ref()
            .map(|o| (o.left, o.by.as_str())),
        Some((2, "0a0b"))
    );
    assert!(note_offer(&mut s, "main", None, 1, None, NOW + 1, NOW + 1));
    assert_eq!(
        s.accounts["main"]
            .offer
            .as_ref()
            .map(|o| (o.left, o.use_by)),
        Some((1, None))
    );
    // A count of one kind keeps its kind, through the state file too; a row
    // an earlier build wrote names none and still reads, and a kind no build
    // names reads as none.
    let weekly = Some(aterm_phase::ResetKind::Weekly);
    assert!(note_offer(
        &mut s,
        "main",
        weekly,
        0,
        None,
        NOW + 2,
        NOW + 2
    ));
    let kind_of = |s: &State| s.accounts["main"].offer.as_ref().map(|o| (o.kind, o.left));
    assert_eq!(kind_of(&s), Some((weekly, 0)));
    let json = s.to_json();
    assert!(json.contains("\"kind\":\"weekly\""), "{json}");
    assert_eq!(
        kind_of(&State::from_json(&json, NOW).expect("reads")),
        Some((weekly, 0))
    );
    for (from, to) in [("\"kind\":\"weekly\",", ""), ("\"weekly\"", "\"hourly\"")] {
        let older = json.replacen(from, to, 1);
        assert_ne!(older, json);
        assert_eq!(
            kind_of(&State::from_json(&older, NOW).expect("reads")),
            Some((None, 0)),
            "{older}"
        );
    }

    // Auth: a row only when the word changes; an older word is dropped;
    // `unseen` is nothing to observe.
    let events = note_auth(&mut s, "work-2", Auth::Ok, NOW);
    assert_eq!((events[0].kind, events[0].why.as_str()), (Kind::Auth, "ok"));
    assert!(events[0].wakes(), "an account usable sooner");
    assert!(note_auth(&mut s, "work-2", Auth::Ok, NOW + 5).is_empty());
    assert_eq!(
        s.accounts["work-2"].auth_at,
        Some(NOW + 5),
        "the look is still dated"
    );
    assert!(
        note_auth(&mut s, "work-2", Auth::SignedOut, NOW).is_empty(),
        "older"
    );
    assert!(note_auth(&mut s, "work-2", Auth::Unseen, NOW + 9).is_empty());
    assert_eq!(s.accounts["work-2"].auth, Auth::Ok);
    let events = note_auth(&mut s, "work-2", Auth::SignedOut, NOW + 9);
    assert_eq!(events[0].why, "signed-out");
    assert!(!events[0].wakes());

    // Sessions: first seen is no move; another account is one.
    assert!(note_session(&mut s, "s-1", "main", None, NOW).is_empty());
    assert!(note_session(&mut s, "s-1", "main", Some("jobs"), NOW + 1).is_empty());
    assert_eq!(s.sessions["s-1"].held.as_deref(), Some("jobs"));
    let events = note_session(&mut s, "s-1", "work-2", None, NOW + 2);
    assert_eq!(
        (
            events[0].kind,
            events[0].account.as_str(),
            events[0].to.as_deref(),
            events[0].sid.as_deref()
        ),
        (Kind::Move, "main", Some("work-2"), Some("s-1"))
    );
    assert_eq!(
        s.sessions["s-1"],
        SessionRow {
            account: "work-2".into(),
            at: NOW + 2,
            held: None
        }
    );
    note_session(&mut s, "s-2", "work-2", None, NOW + 3);
    assert_eq!(s.sessions_on("work-2"), ["s-1", "s-2"]);
    assert!(s.sessions_on("main").is_empty());
    assert!(forget_session(&mut s, "s-1") && !forget_session(&mut s, "s-1"));
    assert_eq!(s.sessions_on("work-2"), ["s-2"]);

    // Each account is stamped as run on when a session was read on it, and
    // the stamp outlives the session's row and is never moved back.
    let ran = |s: &State, label: &str| s.accounts.get(label).and_then(|a| a.ran_at);
    assert_eq!(
        (ran(&s, "main"), ran(&s, "work-2"), ran(&s, "pool-a")),
        (Some(NOW + 1), Some(NOW + 3), None)
    );
    assert!(forget_session(&mut s, "s-2"));
    assert_eq!(
        ran(&s, "work-2"),
        Some(NOW + 3),
        "the row went, the stamp stays"
    );
    note_session(&mut s, "s-3", "work-2", None, NOW - HOUR);
    assert_eq!(
        ran(&s, "work-2"),
        Some(NOW + 3),
        "an older read moves nothing back"
    );
    // It is no observation: an account only ever run on is still unseen.
    let mut only_ran = state();
    note_session(&mut only_ran, "s-9", "pool-a", None, NOW);
    assert!(!only_ran.accounts["pool-a"].observed());
    assert_eq!(cond(&three(), &only_ran, "pool-a", NOW), Condition::Unseen);
}

/// A STAMP IN THE FUTURE IS NOT "RECENT". An age read as `now - t < WINDOW`
/// with no lower bound makes a time stamped ahead of the clock (a clock set
/// back) recent for as long as the clock is behind it: rotation times trip
/// the breaker, a probe claim blocks the probe, and a held sample or wall
/// shadows every real one.
#[test]
fn a_stamp_ahead_of_the_clock_is_not_recent_and_shadows_nothing() {
    let r = three();
    let enrolled = all(&r);
    let ahead = NOW + DAY;

    // THE BREAKER: six rotations stamped tomorrow are not this hour's.
    let mut s = State {
        rotations: vec![ahead; 6],
        ..on_main()
    };
    limited(&mut s, "main", NOW + HOUR);
    assert!(!rotations_spent(&s, &r, NOW));
    assert_eq!(breaker_until(&s, &r, NOW), None);
    assert_eq!(
        advise(&r, &s, &enrolled, &me("main"), &wall(), Mode::Early, NOW),
        Advice::Move {
            to: "work-2".into(),
            ahead: false,
            rotates: true,
            takes_canary: false
        }
    );
    // NEGATIVE CONTROL: the same six stamped a minute ago are.
    let recent = State {
        rotations: vec![NOW - MIN; 6],
        ..s.clone()
    };
    assert!(rotations_spent(&recent, &r, NOW));
    // The rotation that is made drops them from the hour's times.
    rotation(&mut s, Some("main"), "main", "work-2", NOW).expect("rotates");
    assert_eq!(s.rotations, [NOW]);

    // A SAMPLE read while the clock ran a day fast, then the clock is set
    // right: the next real sample is taken, and what it shows counts.
    let mut s = on_main();
    let fast = Sample {
        at: ahead,
        ..sample(10, 20)
    };
    assert_eq!(note_sample(&mut s, "main", fast, ahead).len(), 1);
    let real = Sample {
        at: NOW + HOUR,
        ..sample(99, 99)
    };
    assert_eq!(
        note_sample(&mut s, "main", real.clone(), NOW + HOUR).len(),
        1
    );
    assert_eq!(
        s.accounts["main"].sample.as_ref().map(|x| x.at),
        Some(NOW + HOUR)
    );
    assert_eq!(cond(&r, &s, "main", NOW + HOUR), Condition::Low);
    // The order among real samples is as it was: the same instant again and
    // an older one are dropped.
    assert!(note_sample(&mut s, "main", real, NOW + HOUR).is_empty());
    assert!(note_sample(&mut s, "main", sample(50, 50), NOW + HOUR).is_empty());

    // AN OFFER seen while the clock ran a day fast does not shadow the next
    // one — as it is held, and as it is read back from the file (stamped
    // `now` plus the skew, at every read: a stamp no real offer was newer
    // than, so a spent offer stayed "expiring" until the clock caught up).
    let mut s = state();
    assert!(note_offer(
        &mut s,
        "main",
        None,
        1,
        Some(ahead + HOUR),
        ahead,
        ahead
    ));
    let mut read = State::from_json(&s.to_json(), NOW).expect("reads");
    assert_eq!(
        read.accounts["main"].offer.as_ref().map(|o| o.at),
        Some(NOW + STAMP_SKEW_S)
    );
    for held in [&mut s, &mut read] {
        assert!(
            note_offer(held, "main", None, 0, None, NOW, NOW),
            "a real offer is newer than one stamped ahead of the clock"
        );
        assert_eq!(
            held.accounts["main"]
                .offer
                .as_ref()
                .map(|o| (o.left, o.use_by, o.at)),
            Some((0, None, NOW))
        );
        // The order among real offers is as it was: an older one is dropped.
        assert!(!note_offer(held, "main", None, 5, None, NOW - 1, NOW));
    }

    // A WALL stamped tomorrow does not shadow a real one of that window…
    let mut s = state();
    let noted = |events: Vec<Event>| events.iter().map(|e| e.kind).collect::<Vec<_>>();
    assert_eq!(
        noted(note_wall(
            &mut s,
            "main",
            "five_hour",
            Some(ahead + HOUR),
            ahead,
            ahead
        )),
        [Kind::Wall]
    );
    assert_eq!(
        noted(note_wall(
            &mut s,
            "main",
            "five_hour",
            Some(NOW + 2 * HOUR),
            NOW,
            NOW
        )),
        [Kind::Wall]
    );
    assert_eq!(
        s.accounts["main"].walls["five_hour"],
        Wall {
            until: Some(NOW + 2 * HOUR),
            at: NOW,
            by: "0a0b".into(),
            attempts: 0,
            cleared: false,
        }
    );
    // …and the same wall seen again brings its stamp back to the clock, so
    // it stands on its rung from NOW and not from tomorrow.
    let mut s = state();
    note_wall(&mut s, "main", "overage", None, ahead, ahead);
    assert_eq!(
        s.accounts["main"].limited_until(NOW),
        Some(ahead + 30 * MIN)
    );
    assert!(
        note_wall(&mut s, "main", "overage", None, NOW, NOW).is_empty(),
        "one wall, one row"
    );
    assert_eq!(s.accounts["main"].walls["overage"].at, NOW);
    assert_eq!(s.accounts["main"].limited_until(NOW), Some(NOW + 30 * MIN));
}

// ---------------------------------------------------------------------------
// 5b. The limit reset: the claim, who may claim, and the outcome
// ---------------------------------------------------------------------------

/// A process's kernel start, as a host would hand it over. Invented.
const START: &str = "1790000000.250000";

/// ONE RESET ATTEMPT A WALL, MACHINE-WIDE. The claim is taken once; it is
/// `Mine` only for the same session AND process while in flight; every other
/// asker is held while it is in flight, whichever wall or account they ask
/// for; and once it has an outcome, or is ten minutes old, the wall is
/// attempted for everyone, the claimer included. A later wall is taken over
/// the record; a wall that ends before an attempted one still standing is
/// not. The wall is the same wall under ANY LABEL: the slot is the machine's
/// own login's, and a roster written or a row renamed since the attempt
/// gives that login another label, under which the wall is attempted still.
///
/// NEGATIVE CONTROLS: an outcome told by a session that does not hold the
/// claim (another session, another wall, another account) changes nothing;
/// another account's claim is not in flight on this one; a later wall is
/// taken under the new label as under the old; and a claim nobody could hold
/// writes nothing.
#[test]
fn a_wall_is_claimed_for_a_reset_once_and_the_claim_stays_as_its_record() {
    let until = NOW + 30 * HOUR;
    let take = |s: &mut State, sid: &str, pid: u32, until: i64, now: i64| {
        claim_reset(s, "main", (WEEK_LIMIT, until), sid, pid, START, now)
    };
    let held = ResetClaimed::Held {
        until: NOW + RESET_CLAIM_S,
    };

    let mut s = state();
    assert_eq!(reset_in_flight(&s, "main", NOW), None, "no claim, no hold");
    assert_eq!(take(&mut s, "s-one", 41, until, NOW), ResetClaimed::Taken);
    assert_eq!(
        s.reset,
        Some(ResetClaim {
            account: "main".into(),
            until: Some(until),
            window: Some(WEEK_LIMIT.to_owned()),
            sid: "s-one".into(),
            at: NOW,
            pid: 41,
            start: START.into(),
            done: None,
        })
    );
    let claimed = s.clone();

    // Its own again, while it is in flight; and nobody else's, for any wall
    // or account: another session, this session's next process (by its pid,
    // and by its start), a later wall, another account.
    assert_eq!(
        take(&mut s, "s-one", 41, until, NOW + 5),
        ResetClaimed::Mine
    );
    assert_eq!(take(&mut s, "s-two", 42, until, NOW + 5), held);
    assert_eq!(
        claim_reset(
            &mut s,
            "main",
            (SESSION_LIMIT, until),
            "s-one",
            41,
            START,
            NOW + 5
        ),
        held,
        "same reset time does not retarget the claim"
    );
    assert_eq!(take(&mut s, "s-one", 43, until, NOW + 5), held);
    assert_eq!(
        claim_reset(
            &mut s,
            "main",
            (WEEK_LIMIT, until),
            "s-one",
            41,
            "1790000300.000000",
            NOW + 5
        ),
        held
    );
    assert_eq!(take(&mut s, "s-two", 42, until + 7 * DAY, NOW + 5), held);
    assert_eq!(take(&mut s, "s-one", 41, until + 7 * DAY, NOW + 5), held);
    assert_eq!(
        claim_reset(
            &mut s,
            "work-2",
            (WEEK_LIMIT, until),
            "s-two",
            42,
            START,
            NOW + 5
        ),
        held
    );
    assert_eq!(s, claimed, "none of them wrote anything");
    assert_eq!(
        reset_in_flight(&s, "main", NOW + 5),
        Some(NOW + RESET_CLAIM_S)
    );
    // NEGATIVE CONTROL: it is in flight on its own account alone.
    assert_eq!(reset_in_flight(&s, "work-2", NOW + 5), None);

    // TEN MINUTES, and not a second less: then it is an attempt nobody
    // finished — for the claimer too — and it holds nobody.
    let last = NOW + RESET_CLAIM_S - 1;
    assert_eq!(take(&mut s, "s-one", 41, until, last), ResetClaimed::Mine);
    assert_eq!(take(&mut s, "s-two", 42, until, last), held);
    assert_eq!(reset_in_flight(&s, "main", last), Some(last + 1));
    let lapsed = NOW + RESET_CLAIM_S;
    for (sid, pid) in [("s-one", 41), ("s-two", 42)] {
        assert_eq!(
            take(&mut s, sid, pid, until, lapsed),
            ResetClaimed::Attempted
        );
    }
    assert_eq!(reset_in_flight(&s, "main", lapsed), None);
    assert_eq!(s, claimed, "still nothing written");

    // AN OUTCOME ends it at once, whatever it was: kept is final.
    let mut done = claimed.clone();
    assert_eq!(
        reset_outcome(
            &mut done,
            "main",
            until,
            "s-one",
            &ResetDone::Kept("focus"),
            NOW + 9
        )
        .len(),
        1
    );
    for (sid, pid) in [("s-one", 41), ("s-two", 42)] {
        assert_eq!(
            take(&mut done, sid, pid, until, NOW + 10),
            ResetClaimed::Attempted
        );
    }
    assert_eq!(reset_in_flight(&done, "main", NOW + 10), None);

    // A NEW WALL — a later reset — is taken, over the record of the old one,
    // from a claim that is done and from one that lapsed.
    for (name, from, now) in [("done", &done, NOW + 10), ("lapsed", &claimed, lapsed)] {
        let mut next = from.clone();
        assert_eq!(
            take(&mut next, "s-two", 42, until + 7 * DAY, now),
            ResetClaimed::Taken,
            "{name}"
        );
        assert_eq!(
            next.reset
                .as_ref()
                .map(|c| (c.until, c.sid.as_str(), c.pid, c.at, c.done.as_deref())),
            Some((Some(until + 7 * DAY), "s-two", 42, now, None)),
            "{name}"
        );
        // …and that one is in flight in its turn.
        assert_eq!(
            take(&mut next, "s-one", 41, until + 8 * DAY, now + 1),
            ResetClaimed::Held {
                until: now + RESET_CLAIM_S
            },
            "{name}"
        );
    }
    // A wall of this account that ends BEFORE the attempted one, while that
    // one still stands, does not take the record's place…
    let before = done.clone();
    assert_eq!(
        take(&mut done, "s-two", 42, until - HOUR, NOW + 10),
        ResetClaimed::Attempted
    );
    assert_eq!(done, before);
    // …NEGATIVE CONTROL: once the attempted wall's own reset has come it
    // does.
    assert_eq!(
        take(&mut done.clone(), "s-two", 42, until - HOUR, until),
        ResetClaimed::Taken
    );

    // UNDER ANOTHER LABEL IT IS THE SAME WALL. The attempt was made as
    // `main` — the row of a machine with no roster — and the owner has since
    // written a roster that calls this login `personal`: the wall, and one
    // that ends before it, are attempted still, from a claim that is done
    // and from one that lapsed, for a new process of the claimer's as for
    // anyone.
    let renamed = |s: &State, sid: &str, pid: u32, until: i64, now: i64| {
        let mut s = s.clone();
        let said = claim_reset(
            &mut s,
            "personal",
            (WEEK_LIMIT, until),
            sid,
            pid,
            START,
            now,
        );
        (said, s)
    };
    for (name, from, now) in [("done", &done, NOW + 10), ("lapsed", &claimed, lapsed)] {
        for (sid, pid, wall) in [
            ("s-one", 44, until),
            ("s-two", 42, until),
            ("s-two", 42, until - HOUR),
        ] {
            let (said, after) = renamed(from, sid, pid, wall, now);
            assert_eq!(said, ResetClaimed::Attempted, "{name} {sid} {wall}");
            assert_eq!(&after, from, "{name}: nothing written");
        }
        // NEGATIVE CONTROL: a later wall is taken under the new label.
        let (said, after) = renamed(from, "s-two", 42, until + 7 * DAY, now);
        assert_eq!(said, ResetClaimed::Taken, "{name}");
        assert_eq!(
            after.reset.as_ref().map(|c| c.account.as_str()),
            Some("personal")
        );
    }
    // In flight it is held under the new label as under any, and its own
    // process's still.
    assert_eq!(
        renamed(&claimed, "s-two", 42, until, NOW + 5).0,
        ResetClaimed::Held {
            until: NOW + RESET_CLAIM_S
        }
    );
    assert_eq!(
        renamed(&claimed, "s-one", 41, until, NOW + 5).0,
        ResetClaimed::Mine
    );

    // NEGATIVE CONTROL: an outcome from a session that does not hold the
    // claim changes nothing — another session, another wall, another
    // account — and neither does a second outcome for a claim that has one.
    let mut told = claimed.clone();
    let used = ResetDone::Used {
        kind: aterm_phase::ResetKind::Weekly,
        left: Some(1),
    };
    for (account, wall, sid) in [
        ("main", until, "s-two"),
        ("main", until + 1, "s-one"),
        ("work-2", until, "s-one"),
    ] {
        assert!(
            reset_outcome(&mut told, account, wall, sid, &used, NOW + 9).is_empty(),
            "{account} {wall} {sid}"
        );
    }
    assert_eq!(told, claimed);
    let before = done.clone();
    assert!(reset_outcome(&mut done, "main", until, "s-one", &used, NOW + 11).is_empty());
    assert_eq!(done, before);
    // …and with no claim at all.
    assert!(reset_outcome(&mut state(), "main", until, "s-one", &used, NOW).is_empty());

    // A CLAIM NOBODY COULD HOLD is not taken: no session, no process, a
    // label no roster carries.
    let mut empty = state();
    for (account, sid, pid, start) in [
        ("main", "", 41, START),
        ("main", " \n ", 41, START),
        ("main", "s-one", 0, START),
        ("main", "s-one", 41, ""),
        ("Not A Label", "s-one", 41, START),
    ] {
        assert_eq!(
            claim_reset(
                &mut empty,
                account,
                (WEEK_LIMIT, until),
                sid,
                pid,
                start,
                NOW
            ),
            ResetClaimed::Attempted,
            "{account:?} {sid:?} {pid} {start:?}"
        );
    }
    assert_eq!(empty, state());

    // A CLAIM STAMPED AHEAD OF THE CLOCK (a clock set back) is not in
    // flight: it holds nobody, and its wall is not tried again.
    let ahead = State {
        reset: Some(ResetClaim {
            at: NOW + DAY,
            ..claimed.reset.clone().expect("the claim")
        }),
        ..state()
    };
    assert_eq!(reset_in_flight(&ahead, "main", NOW), None);
    assert_eq!(
        take(&mut ahead.clone(), "s-one", 41, until, NOW),
        ResetClaimed::Attempted
    );

    // A claim an older build left, of no process's: held while in flight,
    // never `Mine`.
    let mut older = claimed.clone();
    let claim = older.reset.as_mut().expect("the claim");
    (claim.pid, claim.start) = (0, String::new());
    assert_eq!(take(&mut older, "s-one", 41, until, NOW + 5), held);
}

/// A reset used or a limit found lifted clears only its claimed raw wall,
/// with its timestamp retained in
/// that wall's slot. Other walls still limit the account. The row is
/// `reset-used` only for `Used`, its reason the count of the kind spent in
/// the vendor's words (Claude Code 2.1.304 counts each kind apart); a limit
/// that lifted by itself is `reset-kept`. Each outcome is recorded once and
/// wakes the waiting tabs. Whatever the kind spent, only the claimed window
/// is cleared: a full reset's line does not prove every other quota refilled.
///
/// NEGATIVE CONTROLS: before any outcome the session at that row is rotated;
/// a reset KEPT or FAILED changes no wall and clears nothing, and the same
/// session is still rotated; and a reason is one bounded word, never a path
/// or an address.
#[test]
fn a_reset_used_or_a_limit_lifted_clears_only_its_claimed_window() {
    use aterm_phase::ResetKind;
    let r = three();
    let enrolled = all(&r);
    let written = NOW - HOUR;
    let until = NOW + 30 * HOUR;
    let rotate = Advice::Move {
        to: "work-2".into(),
        ahead: false,
        rotates: true,
        takes_canary: false,
    };
    let ask = |s: &State| {
        advise(
            &r,
            s,
            &enrolled,
            &me("main"),
            &Point::Wall {
                window: WEEK_LIMIT.to_owned(),
                at: Some(written),
            },
            Mode::Wall,
            NOW,
        )
    };
    let limited_now = |s: &State| matches!(cond(&r, s, "main", NOW), Condition::Limited { .. });

    // Generic, model-specific and unknown walls at shared reset times,
    // and the claim.
    let mut walled = on_main();
    for (window, end) in [
        (WEEK_LIMIT, until),
        (SESSION_LIMIT, NOW + 2 * HOUR),
        ("seven_day_sonnet", until),
        ("future_quota", until),
    ] {
        assert_eq!(
            note_wall(&mut walled, "main", window, Some(end), written, written).len(),
            1
        );
    }
    assert_eq!(
        claim_reset(
            &mut walled,
            "main",
            (WEEK_LIMIT, until),
            "s-one",
            41,
            START,
            NOW - MIN
        ),
        ResetClaimed::Taken
    );
    // NEGATIVE CONTROL: with the reset only claimed, the wall stands and the
    // session at its row is rotated.
    assert!(limited_now(&walled));
    assert_eq!(ask(&walled), rotate);

    let used = |kind: ResetKind, left: u32| ResetDone::Used {
        kind,
        left: Some(left),
    };
    for (done, kind, why) in [
        (
            used(ResetKind::Weekly, 1),
            Kind::ResetUsed,
            "1 weekly reset left",
        ),
        (
            used(ResetKind::Session, 0),
            Kind::ResetUsed,
            "no five-hour resets left",
        ),
        (
            used(ResetKind::Full, 2),
            Kind::ResetUsed,
            "2 full resets left",
        ),
        (ResetDone::Lifted, Kind::ResetKept, "lifted"),
    ] {
        let mut s = walled.clone();
        let rows = reset_outcome(&mut s, "main", until, "s-one", &done, NOW);
        assert_eq!(
            rows.iter()
                .map(|e| (
                    e.t,
                    e.kind,
                    e.account.as_str(),
                    e.sid.as_deref(),
                    e.why.as_str()
                ))
                .collect::<Vec<_>>(),
            [(NOW, kind, "main", Some("s-one"), why)],
            "{done:?}: the count is its kind's, in the vendor's words"
        );
        assert!(rows[0].wakes(), "{done:?}");
        let main = &s.accounts["main"];
        assert!(main.walls[WEEK_LIMIT].cleared, "{done:?}: the claimed wall");
        for window in [SESSION_LIMIT, "seven_day_sonnet", "future_quota"] {
            assert_eq!(
                main.walls[window], walled.accounts["main"].walls[window],
                "{done:?}: {window}"
            );
        }
        assert_eq!(
            main.cleared_at, None,
            "{done:?}: no account-wide owner clear"
        );
        assert_eq!(main.cleared_since(WEEK_LIMIT, written, NOW), Some(NOW));
        assert_eq!(main.cleared_since(SESSION_LIMIT, written, NOW), None);
        assert_eq!(
            s.reset.as_ref().and_then(|c| c.done.as_deref()),
            Some(done.word()),
            "{done:?}"
        );
        // Nothing else moved: the claim's word and the account's record.
        let mut same = walled.clone();
        same.reset = s.reset.clone();
        same.accounts = s.accounts.clone();
        assert_eq!(s, same, "{done:?}: active, the proof and the pin are left");

        // An unrelated standing wall still refuses an account resume. With
        // only the cleared window left, its old row can resume in place.
        assert!(limited_now(&s), "{done:?}: unrelated walls still stand");
        assert_eq!(ask(&s), rotate, "{done:?}: no resume through another wall");
        let mut only_claimed = s.clone();
        only_claimed
            .accounts
            .get_mut("main")
            .expect("account")
            .walls
            .retain(|window, _| window == WEEK_LIMIT);
        assert_eq!(
            ask(&only_claimed),
            Advice::Resume { cleared_at: NOW },
            "{done:?}"
        );
        let before = s.clone();
        let own = Roster::own();
        assert_eq!(
            advise(
                &own,
                &only_claimed,
                &all(&own),
                &me("main"),
                &Point::Wall {
                    window: WEEK_LIMIT.to_owned(),
                    at: Some(written)
                },
                Mode::Wall,
                NOW
            ),
            Advice::Resume { cleared_at: NOW },
            "{done:?}"
        );
        // The scoped clear survives disk, ignores its old transcript row,
        // and does not suppress a genuinely new row from the same second.
        let mut read = State::from_json(&s.to_json(), NOW).expect("scoped clear reads");
        assert_eq!(read, s);
        assert!(note_wall(&mut read, "main", WEEK_LIMIT, Some(until), written, NOW + 1).is_empty());
        assert_eq!(read, s, "an older row cannot resurrect the claimed wall");
        assert_eq!(
            note_wall(&mut read, "main", WEEK_LIMIT, Some(until), NOW, NOW + 1).len(),
            1
        );
        assert!(!read.accounts["main"].walls[WEEK_LIMIT].cleared);
        assert_eq!(read.accounts["main"].walls[WEEK_LIMIT].at, NOW);

        // Recorded once.
        assert!(
            reset_outcome(
                &mut s,
                "main",
                until,
                "s-one",
                &ResetDone::Failed("late"),
                NOW + 1
            )
            .is_empty(),
            "{done:?}"
        );
        assert_eq!(s, before, "{done:?}");
    }

    // An old on-disk claim has no raw scope. Its outcome is recorded, but
    // cannot clear even a wall sharing its reset time.
    let mut unscoped = walled.clone();
    unscoped.reset.as_mut().expect("claim").window = None;
    reset_outcome(
        &mut unscoped,
        "main",
        until,
        "s-one",
        &ResetDone::Used {
            kind: ResetKind::Full,
            left: None,
        },
        NOW,
    );
    assert_eq!(unscoped.accounts, walled.accounts);
    assert_eq!(
        unscoped.reset.as_ref().and_then(|c| c.done.as_deref()),
        Some("used")
    );

    // A newer different period, or the old wall found standing past its
    // grace, must survive a delayed answer to this claim.
    for (new_until, observed) in [(until + DAY, NOW - 1), (until, until + RESET_GRACE_S + 1)] {
        let mut newer = walled.clone();
        note_wall(
            &mut newer,
            "main",
            WEEK_LIMIT,
            Some(new_until),
            observed,
            observed,
        );
        assert_eq!(newer.accounts["main"].walls[WEEK_LIMIT].at, observed);
        let accounts_before = newer.accounts.clone();
        reset_outcome(
            &mut newer,
            "main",
            until,
            "s-one",
            &ResetDone::Lifted,
            observed + 1,
        );
        assert_eq!(newer.accounts, accounts_before);
    }

    // KEPT AND FAILED CHANGE NO WALL, and nor does a reset spent whose line
    // says the wall still stands (recorded as used). The reason is the
    // caller's word; a vendor line's head and no more.
    for (done, kind, why) in [
        (
            ResetDone::Walled {
                kind: ResetKind::Fable,
                left: 0,
            },
            Kind::ResetUsed,
            "no Fable resets left, still walled",
        ),
        (ResetDone::Kept("early"), Kind::ResetKept, "early"),
        (ResetDone::Failed("login"), Kind::ResetFailed, "login"),
        (
            ResetDone::Failed("That one was already taken · nothing was used · try later"),
            Kind::ResetFailed,
            "That one was already taken",
        ),
    ] {
        let mut s = walled.clone();
        let rows = reset_outcome(&mut s, "main", until, "s-one", &done, NOW);
        assert_eq!(
            rows,
            [Event::new(NOW, kind, "main").with_sid("s-one").because(why)],
            "{done:?}"
        );
        assert!(rows[0].wakes(), "{done:?}: the hold on the others ends");
        assert_eq!(
            s.accounts, walled.accounts,
            "{done:?}: no wall, no clearing"
        );
        assert!(limited_now(&s), "{done:?}");
        assert_eq!(ask(&s), rotate, "{done:?}");
        assert_eq!(
            s.reset.as_ref().and_then(|c| c.done.as_deref()),
            Some(done.word()),
            "{done:?}"
        );
    }

    // A REASON IS ONE BOUNDED WORD: whatever it is handed, no path, no
    // address and no second line reaches the ledger.
    let long = "é".repeat(400);
    for (given, kept) in [
        ("/logins/work-2/state.json could not be read", String::new()),
        ("see ~nobody for more", "see".to_string()),
        ("ask who@there", "ask who".to_string()),
        ("C:\\logins\\x", "C:".to_string()),
        ("two\nlines\there", "two lines here".to_string()),
        ("", String::new()),
        (long.as_str(), "é".repeat(WORD_MAX / 2)),
    ] {
        for done in [ResetDone::Kept(given), ResetDone::Failed(given)] {
            let rows = reset_outcome(&mut walled.clone(), "main", until, "s-one", &done, NOW);
            assert_eq!(rows[0].why, kept, "{given:?}");
            assert!(rows[0].why.len() <= WORD_MAX);
        }
    }
}

/// A RESET IS ALLOWED ONLY ON THIS MACHINE'S OWN LOGIN, with the machine on
/// that account or on none, and no other login on record as run within 25
/// hours before the process started: the vendor posts the claim to the home
/// profile's organization, which another login's process may have moved.
///
/// NEGATIVE CONTROLS: a row with a `dir`, a label nobody listed and another
/// `active` each refuse by their own word; another row's run 24 h before
/// the start refuses where 26 h passes; and the account's OWN run refuses
/// nothing.
#[test]
fn a_reset_is_allowed_only_on_the_machines_own_login_with_no_other_login_on_record() {
    let r = three();
    let started = NOW - HOUR;
    let ok = |s: &State, label: &str| reset_allowed(&r, s, label, started);

    // The default row alone: with nothing active, and with itself active.
    assert_eq!(ok(&state(), "main"), Ok(()));
    assert_eq!(ok(&on_main(), "main"), Ok(()));
    // …and the one row a machine with no roster file is judged by.
    let own = Roster::own();
    assert_eq!(
        own,
        roster(&[("main", Class::Dedicated, None)]),
        "this machine's own login, as the first `add` lists it"
    );
    assert_eq!(
        Roster::parse(&append_row("", "main", Class::Dedicated, None).expect("a row")),
        Ok(own.clone())
    );
    assert_eq!(reset_allowed(&own, &state(), "main", started), Ok(()));

    // A row with a dir; a label that is no row; no roster at all.
    assert_eq!(ok(&state(), "work-2"), Err("not-default"));
    assert_eq!(ok(&state(), "pool-a"), Err("not-default"));
    assert_eq!(ok(&state(), "nobody"), Err("not-listed"));
    assert_eq!(
        reset_allowed(&Roster::default(), &state(), "main", started),
        Err("not-listed")
    );
    // The machine is on another account: a rotation or a pin moved it.
    let moved = State {
        active: Some("work-2".into()),
        ..state()
    };
    assert_eq!(ok(&moved, "main"), Err("active-other"));

    // ANOTHER LOGIN RAN. Its session is gone; its stamp is what is asked.
    let ran = |label: &str, at: i64| {
        let mut s = state();
        note_session(&mut s, "s-x", label, None, at);
        assert!(forget_session(&mut s, "s-x"));
        s
    };
    assert_eq!(
        ok(&ran("work-2", started - 24 * HOUR), "main"),
        Err("other-login")
    );
    assert_eq!(ok(&ran("work-2", started - 26 * HOUR), "main"), Ok(()));
    // The edge is 25 hours, to the second.
    assert_eq!(
        ok(&ran("work-2", started - RESET_PROFILE_S), "main"),
        Ok(())
    );
    assert_eq!(
        ok(&ran("work-2", started - RESET_PROFILE_S + 1), "main"),
        Err("other-login")
    );
    // One that ran AFTER this process started counts too.
    assert_eq!(
        ok(&ran("pool-a", started + MIN), "main"),
        Err("other-login")
    );
    // An account the roster no longer lists ran all the same.
    assert_eq!(
        ok(&ran("gone-9", started - HOUR), "main"),
        Err("other-login")
    );
    // NEGATIVE CONTROL: the account's own run refuses nothing.
    assert_eq!(ok(&ran("main", started - HOUR), "main"), Ok(()));

    // A session STILL ON RECORD on another account says the same with no
    // stamp at all (a state an older build wrote).
    let on_record = |at: i64| {
        let mut s = state();
        s.sessions.insert(
            "s-y".into(),
            SessionRow {
                account: "pool-a".into(),
                at,
                held: None,
            },
        );
        s
    };
    assert_eq!(
        ok(&on_record(started - 2 * HOUR), "main"),
        Err("other-login")
    );
    assert_eq!(ok(&on_record(started - 26 * HOUR), "main"), Ok(()));

    // The first refusal is the answer: the row before the machine, the
    // machine before the other login.
    let mut all_three = ran("pool-a", started);
    all_three.active = Some("pool-a".into());
    assert_eq!(ok(&all_three, "work-2"), Err("not-default"));
    assert_eq!(ok(&all_three, "main"), Err("active-other"));
}

// ---------------------------------------------------------------------------
// 6. The ledger row
// ---------------------------------------------------------------------------

#[test]
fn a_ledger_row_round_trips_as_one_bounded_line() {
    for kind in Kind::ALL {
        let event = Event::new(NOW, kind, "main")
            .with_to("work-2")
            .with_sid("s-1a2b")
            .because("five_hour until 1790003600");
        let line = event.to_json_line();
        assert!(!line.contains('\n'), "{line}");
        assert_eq!(Event::of_json_line(&line), Some(event), "{}", kind.as_str());
        assert_eq!(Kind::parse(kind.as_str()), Some(kind));
    }
    assert_eq!(
        Kind::ALL.map(Kind::as_str),
        [
            "wall",
            "sample",
            "rotate",
            "proven",
            "struck",
            "move",
            "probe",
            "reset-used",
            "reset-kept",
            "reset-failed",
            "auth",
            "use",
            "unpin",
            "no-tap",
            "credits",
            "warned",
            "renewed",
            "login-ended",
            "wrong-account"
        ]
    );

    // Keys sorted; what is absent is `null`.
    let bare = Event::new(NOW, Kind::Proven, "work-2");
    assert_eq!(
        bare.to_json_line(),
        format!(
            "{{\"account\":\"work-2\",\"kind\":\"proven\",\"sid\":null,\"t\":{NOW},\"to\":null,\
             \"why\":\"\"}}"
        )
    );
    assert_eq!(Event::of_json_line(&bare.to_json_line()), Some(bare));

    // Bounded and on one line, whatever it is handed: a reason quoted from
    // a screen cannot break the row or grow it.
    let long = "é".repeat(4000);
    let hostile = Event {
        t: NOW,
        kind: Kind::Struck,
        account: format!("main\n{long}"),
        to: Some(long.clone()),
        sid: Some(format!("s-1\u{2028}{long}")),
        why: format!("failed:\r\n\t{long}"),
    };
    let line = hostile.to_json_line();
    assert!(!line.contains(['\n', '\r', '\u{2028}']));
    assert!(line.len() < MAX_EVENT_LINE_BYTES, "{}", line.len());
    let back = Event::of_json_line(&line).expect("still a row");
    assert!(back.account.len() <= LABEL_MAX && back.why.len() <= WHY_MAX);
    assert!(back.to.is_some_and(|t| t.len() <= LABEL_MAX));
    assert!(back.sid.is_some_and(|s| s.len() <= WORD_MAX));
    assert!(back.why.starts_with("failed: é"), "{}", back.why);
    // The builders bound what they take too.
    let built = Event::new(NOW, Kind::Wall, &long)
        .with_to("")
        .with_sid(" \n ")
        .because(&long);
    assert!(built.account.len() <= LABEL_MAX && built.why.len() <= WHY_MAX);
    assert_eq!(
        (built.to, built.sid),
        (None, None),
        "an empty word is no word"
    );
}

#[test]
fn a_line_that_is_not_a_row_of_this_ledger_is_no_event() {
    let good = Event::new(NOW, Kind::Wall, "main").to_json_line();
    assert!(Event::of_json_line(&good).is_some());
    assert!(
        Event::of_json_line(&format!("  {good}  ")).is_some(),
        "the line's own ends"
    );
    let hostile = [
        String::new(),
        "not json".to_string(),
        "[]".to_string(),
        "null".to_string(),
        good[..good.len() - 3].to_string(),
        good.replace("\"wall\"", "\"wallop\""),
        good.replace("\"wall\"", "7"),
        good.replace(&NOW.to_string(), "\"now\""),
        good.replace(&NOW.to_string(), "1.5"),
        good.replace("\"main\"", "null"),
        good.replace("\"main\"", "\"\""),
        "{\"kind\":\"wall\"}".to_string(),
        format!("{good}{good}"),
        "[".repeat(10_000),
        format!(
            "{{\"account\":\"main\",\"kind\":\"wall\",\"t\":1,\"why\":\"{}\"}}",
            "x".repeat(MAX_EVENT_LINE_BYTES)
        ),
    ];
    for line in &hostile {
        assert_eq!(
            Event::of_json_line(line),
            None,
            "{:?}",
            &line[..line.len().min(60)]
        );
    }
    // Unknown keys and wrong-shaped optional ones are tolerated.
    let extra = "{\"account\":\"main\",\"kind\":\"use\",\"t\":5,\"to\":3,\"sid\":[],\"x\":{}}";
    assert_eq!(
        Event::of_json_line(extra),
        Some(Event::new(5, Kind::Use, "main"))
    );
}

#[test]
fn a_row_wakes_waiting_tabs_only_when_an_account_is_usable_sooner() {
    let wakes = |kind: Kind, why: &str| Event::new(NOW, kind, "main").because(why).wakes();
    // Every reset outcome wakes: used ends the walls, and kept and failed
    // end the hold a reset in flight put on every move off the account.
    let waking = [
        Kind::Use,
        Kind::Unpin,
        Kind::Proven,
        Kind::Struck,
        Kind::ResetUsed,
        Kind::ResetKept,
        Kind::ResetFailed,
        Kind::Renewed,
    ];
    for kind in waking {
        assert!(wakes(kind, ""), "{}", kind.as_str());
        assert!(wakes(kind, "lifted"), "{}", kind.as_str());
    }
    assert!(wakes(Kind::Auth, "ok"));
    // NEGATIVE CONTROLS.
    assert!(!wakes(Kind::Auth, "signed-out"));
    let quiet = [
        Kind::Wall,
        Kind::Sample,
        Kind::Rotate,
        Kind::Move,
        Kind::Probe,
        Kind::NoTap,
        Kind::Credits,
        Kind::Warned,
        Kind::LoginEnded,
        Kind::WrongAccount,
    ];
    for kind in quiet {
        assert!(!wakes(kind, "ok"), "{}", kind.as_str());
    }
    // Every kind is in one list or the other, but for the auth word.
    assert_eq!(waking.len() + quiet.len() + 1, Kind::ALL.len());
}

// ---------------------------------------------------------------------------
// 7. The store
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
}

#[test]
fn the_store_keeps_the_state_private_and_reads_back_what_it_wrote() {
    let dir = tmp("store");
    let store = Store::at(dir.path());
    assert_eq!(store.dir, dir.path().join("accounts"));
    assert_eq!(store.state_file(), store.dir.join("state.json"));
    assert_eq!(store.ledger_path(), store.dir.join("ledger.jsonl"));
    assert_eq!(store.wake_path(), store.dir.join("wake"));
    assert_eq!(store.logins_dir(), store.dir.join("logins"));
    assert_eq!(
        store.login_dir("work-2"),
        Some(store.dir.join("logins").join("work-2"))
    );
    for bad in ["", "..", "../x", "a/b", "Work", "/abs"] {
        assert_eq!(store.login_dir(bad), None, "{bad:?} is never a path");
    }

    // Nothing there: the empty state, no note, nothing written by a read —
    // and a machine id, minted.
    let (first, note) = store.load(NOW).expect("no file is ours");
    assert_eq!(note, None);
    assert!(!store.dir.exists(), "a read makes nothing");
    assert_eq!(first.machine.len(), 16);
    assert!(aterm_codec::hex::is_lower(&first.machine, 16));
    assert_eq!(
        State {
            machine: String::new(),
            ..first.clone()
        },
        State::default()
    );

    // Saved, it reads back whole, and the id is the one kept from then on.
    let mut s = full_state();
    s.machine = first.machine.clone();
    store.save(&s).expect("saves");
    assert_eq!(store.load(NOW), Ok((s.clone(), None)));
    assert_eq!(
        store.load(NOW).expect("ours").0.machine,
        first.machine,
        "minted once"
    );
    #[cfg(unix)]
    {
        assert_eq!(mode(&store.dir), 0o700);
        assert_eq!(mode(&store.state_file()), 0o600);
    }
    let on_disk = std::fs::read_to_string(store.state_file()).expect("reads");
    assert_eq!(on_disk, format!("{}\n", s.to_json()));
    assert_eq!(
        std::fs::read_dir(&store.dir).expect("lists").count(),
        1,
        "the temp file is renamed away"
    );
}

#[test]
fn a_torn_state_file_is_an_empty_state_and_a_note_never_a_panic() {
    let dir = tmp("torn");
    let store = Store::at(dir.path());
    store.ensure().expect("dir");
    let good = full_state().to_json();
    // Not JSON: a file cut short, holding nothing a save could lose.
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (good.as_bytes()[..good.len() / 3].to_vec(), "not JSON"),
        (Vec::new(), "not JSON"),
        (vec![0xff, 0xfe, 0x00], "not UTF-8"),
        ("[".repeat(50_000).into_bytes(), "not JSON"),
    ];
    for (bytes, needle) in cases {
        std::fs::write(store.state_file(), &bytes).expect("write");
        let (s, note) = store
            .load(NOW)
            .unwrap_or_else(|why| panic!("{needle}: replaceable, not refused: {why}"));
        assert!(note.is_some(), "{needle}: a note");
        assert_eq!(
            State {
                machine: String::new(),
                ..s.clone()
            },
            State::default()
        );
        assert_eq!(s.machine.len(), 16, "an empty state still gets an id");
    }
    // The next save replaces the torn file.
    let (s, _) = store.load(NOW).expect("replaceable");
    store.save(&s).expect("saves");
    assert_eq!(store.load(NOW), Ok((s, None)));
}

/// A STATE FILE THAT IS NOT OURS IS NEVER SAVED OVER. One of another schema
/// (a newer build's, read here by an older one), one that cannot be read,
/// one too large: `load` REFUSES, by name, instead of handing back an empty
/// state for the next save to write over the pin, the walls and the
/// signed-out words that file may hold.
///
/// NEGATIVE CONTROL: the same directory with this build's own file loads.
#[test]
fn a_state_file_that_is_not_ours_is_a_refusal_and_never_an_empty_state() {
    let dir = tmp("not-ours");
    let store = Store::at(dir.path());
    store.ensure().expect("dir");
    let ours = full_state();
    let good = ours.to_json();
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (
            good.replacen("\"schema\":1", "\"schema\":2", 1)
                .into_bytes(),
            "schema 2; this build reads schema 1",
        ),
        (b"{\"schema\":7}".to_vec(), "schema 7"),
        (b"{}".to_vec(), "names no schema"),
        (b"[1, 2]".to_vec(), "not a JSON object"),
        (
            vec![b' '; usize::try_from(MAX_STATE_BYTES).expect("fits") + 1],
            "larger than",
        ),
    ];
    for (bytes, needle) in cases {
        std::fs::write(store.state_file(), &bytes).expect("write");
        assert!(store.load(NOW).is_err(), "{needle}: not ours to replace");
        assert_eq!(
            std::fs::read(store.state_file()).expect("read"),
            bytes,
            "{needle}: the file is as it was"
        );
    }
    // A file that cannot be read: a directory in its place stands in for a
    // permission or an I/O error.
    let other = Store {
        dir: dir.path().join("odd"),
        ..Store::at(dir.path())
    };
    std::fs::create_dir_all(other.state_file()).expect("mkdir");
    assert!(
        other
            .load(NOW)
            .is_err_and(|why| why.starts_with("state.json: "))
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::write(store.state_file(), &good).expect("write");
        let unreadable = std::fs::Permissions::from_mode(0o000);
        std::fs::set_permissions(store.state_file(), unreadable).expect("chmod");
        // Root reads through the mode: only where the mode binds is this
        // the refusal.
        if std::fs::read(store.state_file()).is_err() {
            assert!(store.load(NOW).is_err(), "a permission is not a torn file");
        }
        let readable = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(store.state_file(), readable).expect("chmod");
    }

    // NEGATIVE CONTROL.
    std::fs::write(store.state_file(), &good).expect("write");
    assert_eq!(store.load(NOW), Ok((ours, None)));
}

#[test]
fn the_lock_excludes_a_second_holder_and_is_free_the_moment_it_drops() {
    let dir = tmp("lock");
    let store = Store::at(dir.path());
    let held = store.acquire().expect("the first holder");
    #[cfg(unix)]
    assert_eq!(mode(&store.dir.join("accounts.lock")), 0o600);
    // A second handle on the same file does not get it.
    assert!(store.try_acquire().expect("tries").is_none());
    assert!(
        Store::at(dir.path())
            .try_acquire()
            .expect("tries")
            .is_none()
    );

    // A blocking taker waits for the drop: it cannot have the lock while
    // the first holder does, whenever its thread happens to run.
    let (tx, rx) = std::sync::mpsc::channel();
    let other = store.clone();
    let waiter = std::thread::spawn(move || {
        let lock = other.acquire().expect("waits, then holds");
        tx.send(()).expect("reports");
        drop(lock);
    });
    assert!(rx.try_recv().is_err(), "held: the waiter has not got it");
    drop(held);
    waiter.join().expect("the waiter ends");
    assert_eq!(rx.recv(), Ok(()));

    // Free again, and exclusive again.
    let again = store
        .try_acquire()
        .expect("tries")
        .expect("free once dropped");
    assert!(store.try_acquire().expect("tries").is_none());
    drop(again);
    assert!(store.try_acquire().expect("tries").is_some());
}

#[test]
fn the_ledger_appends_whole_rows_and_is_cut_to_its_newest() {
    let dir = tmp("ledger");
    let store = Store::at(dir.path());
    assert_eq!(store.tail(10).expect("no file is no rows"), []);
    store.append(&[]).expect("nothing to write");
    assert!(
        !store.ledger_path().exists(),
        "an empty append makes nothing"
    );

    let rows = [
        Event::new(NOW, Kind::Wall, "main").because("five_hour until 5"),
        Event::new(NOW + 1, Kind::Rotate, "main")
            .with_to("work-2")
            .with_sid("s-1"),
    ];
    store.append(&rows).expect("appends");
    store
        .append(&[Event::new(NOW + 2, Kind::Proven, "work-2")])
        .expect("appends");
    #[cfg(unix)]
    assert_eq!(mode(&store.ledger_path()), 0o600);
    let text = std::fs::read_to_string(store.ledger_path()).expect("reads");
    assert_eq!(text.lines().count(), 3);
    assert!(text.ends_with('\n'));
    let tail = store.tail(10).expect("reads");
    assert_eq!(tail.len(), 3);
    assert_eq!(&tail[..2], &rows);
    assert_eq!(
        store.tail(1).expect("reads")[0].kind,
        Kind::Proven,
        "the newest"
    );

    // A line that is not a row is skipped, not an error.
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(store.ledger_path())
            .expect("opens");
        f.write_all(b"not a row\n{\"kind\":\"nope\"}\n")
            .expect("writes");
    }
    assert_eq!(store.tail(10).expect("reads").len(), 3);

    // Past its size the ledger is cut back to its newest rows.
    let why = "w".repeat(WHY_MAX);
    let many: Vec<Event> = (0..6000_i64)
        .map(|i| Event::new(NOW + 10 + i, Kind::Sample, "main").because(&why))
        .collect();
    assert!(
        many.iter()
            .map(|e| e.to_json_line().len() as u64 + 1)
            .sum::<u64>()
            > LEDGER_MAX_BYTES
    );
    store.append(&many).expect("appends and cuts");
    let kept = store.tail(usize::MAX).expect("reads");
    assert_eq!(kept.len(), LEDGER_KEEP_ROWS);
    assert_eq!(
        kept.last().map(|e| e.t),
        Some(NOW + 10 + 5999),
        "the newest row is kept"
    );
    assert_eq!(kept.first().map(|e| e.t), Some(NOW + 10 + 6000 - 1024));
    assert!(std::fs::metadata(store.ledger_path()).expect("stat").len() <= LEDGER_MAX_BYTES);
    // NEGATIVE CONTROL: a small append after the cut cuts nothing more.
    store
        .append(&[Event::new(NOW + 9999, Kind::Use, "main")])
        .expect("appends");
    assert_eq!(
        store.tail(usize::MAX).expect("reads").len(),
        LEDGER_KEEP_ROWS + 1
    );
}

#[test]
fn the_wake_marker_is_a_new_file_at_every_touch() {
    let dir = tmp("wake");
    let store = Store::at(dir.path());
    assert!(!store.wake_path().exists());
    let recording = aterm_durable::seam::Recording::start();
    store.touch_wake().expect("touches");
    assert_eq!(
        recording.take(),
        [aterm_durable::Flush::Ordered],
        "a wake marker is only ordered"
    );
    assert!(store.wake_path().is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        assert_eq!(mode(&store.wake_path()), 0o600);
        // Replaced by a rename, not written in place: the file's identity
        // is what a watcher wakes on.
        let ino = |s: &Store| std::fs::metadata(s.wake_path()).expect("stat").ino();
        let first = ino(&store);
        store.touch_wake().expect("touches again");
        assert_ne!(ino(&store), first);
    }
    assert_eq!(
        std::fs::read_dir(&store.dir).expect("lists").count(),
        1,
        "no temp file is left beside it"
    );
}

// ---------------------------------------------------------------------------
// 8. The process's login and the recorder
// ---------------------------------------------------------------------------

use crate::harness::upgrade::StandingWall;

fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> ProcessLogin {
    ProcessLogin::of_env(pairs.iter().copied())
}

/// A process on this machine's own login, default home, nothing else set.
fn stored() -> ProcessLogin {
    ProcessLogin {
        login: None,
        default_home: true,
        stored_login: true,
    }
}

#[test]
fn a_process_login_is_read_from_its_own_environment_and_nothing_else() {
    // Nothing set, and nothing that matters set: this machine's own login.
    assert_eq!(env_of(&[]), stored());
    let unrelated = [
        ("PATH", "/usr/bin"),
        ("HOME", "/home/someone"),
        ("ANTHROPIC_MODEL", "opus"),
        ("ANTHROPIC_API_KEY_OLD", "k"),
        ("anthropic_api_key", "k"),
        ("XCLAUDE_CODE_USE_BEDROCK", "1"),
        ("CLAUDE_CODE_USE_BEDROCK_TOO", "1"),
        ("CLAUDE_CONFIG_DIR_BACKUP", "/elsewhere"),
        ("CLAUDE_SECURESTORAGE_CONFIG_DIR_OLD", "/logins/work-2"),
    ];
    assert_eq!(env_of(&unrelated), stored());

    // THE LOGIN, VERBATIM: a space and a trailing slash survive, and no
    // spelling is folded onto another. Empty is the default login.
    for dir in [
        "/logins/work-2",
        "/logins/work 2/",
        " /x ",
        "relative/dir",
        "~",
    ] {
        assert_eq!(
            env_of(&[(LOGIN_ENV, dir)]),
            ProcessLogin {
                login: Some(dir.to_string()),
                ..stored()
            },
            "{dir:?}"
        );
    }
    assert_eq!(env_of(&[(LOGIN_ENV, "")]), stored());

    // THE HOME: any value moves it; empty does not.
    assert_eq!(
        env_of(&[(HOME_ENV, "/elsewhere")]),
        ProcessLogin {
            default_home: false,
            ..stored()
        }
    );
    assert_eq!(env_of(&[(HOME_ENV, "")]), stored());

    // NOT A STORED LOGIN: each variable that gives the process a credential,
    // a provider or a route of its own — the seven the design's rule 1
    // stands on by name, then every one of both lists.
    let named = [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "ANTHROPIC_BASE_URL",
        // The vendor's own lists beside those: a gateway's selector and its
        // token, and a provider the process's host manages.
        "CLAUDE_CODE_USE_GATEWAY",
        "CLAUDE_CODE_GATEWAY_TOKEN_FILE_DESCRIPTOR",
        "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
        "CLAUDE_CODE_HOST_AUTH_ENV_VAR",
    ];
    let listed = CREDENTIAL_KEYS
        .iter()
        .chain(crate::harness::netwatch::ROUTE_KEYS);
    let prefixed = "CLAUDE_CODE_PROXY_URL";
    assert!(
        crate::harness::netwatch::ROUTE_PREFIXES
            .iter()
            .any(|p| prefixed.starts_with(p))
    );
    for name in named.iter().chain(listed).chain([&prefixed]) {
        // SET MEANS NOT EMPTY: a zero and a space are set.
        for value in ["sk-not-a-key", "1", "0", " ", "false"] {
            assert_eq!(
                env_of(&[(name, value)]),
                ProcessLogin {
                    stored_login: false,
                    ..stored()
                },
                "{name}={value:?}"
            );
        }
        // NEGATIVE CONTROL: set and empty is not set.
        assert_eq!(env_of(&[(name, "")]), stored(), "{name}=");
    }

    // All three at once, in any order, each read for itself.
    let all = [
        ("ANTHROPIC_API_KEY", "k"),
        (HOME_ENV, "/elsewhere"),
        (LOGIN_ENV, "/logins/pool-a"),
    ];
    assert_eq!(
        env_of(&all),
        ProcessLogin {
            login: Some("/logins/pool-a".to_string()),
            default_home: false,
            stored_login: false,
        }
    );

    // A NAME GIVEN TWICE. The home and a credential: either one counts.
    assert!(!env_of(&[(HOME_ENV, ""), (HOME_ENV, "/elsewhere")]).default_home);
    assert!(!env_of(&[("ANTHROPIC_API_KEY", "k"), ("ANTHROPIC_API_KEY", "")]).stored_login);
    // The login, with two values: it cannot be told, so it is the first and
    // the process is no stored login — also when one of the two is empty.
    for (a, b) in [
        ("/logins/work-2", "/logins/pool-a"),
        ("/logins/work-2", ""),
        ("", "/logins/work-2"),
        ("/logins/work-2", "/logins/work-2/"),
    ] {
        let twice = env_of(&[(LOGIN_ENV, a), (LOGIN_ENV, b)]);
        assert_eq!(
            twice,
            ProcessLogin {
                login: Some(a.to_string()).filter(|a| !a.is_empty()),
                default_home: true,
                stored_login: false,
            },
            "{a:?} then {b:?}"
        );
    }
    // NEGATIVE CONTROL: the same value twice is one login.
    assert_eq!(
        env_of(&[(LOGIN_ENV, "/logins/work-2"), (LOGIN_ENV, "/logins/work-2")]),
        ProcessLogin {
            login: Some("/logins/work-2".to_string()),
            ..stored()
        }
    );
}

/// THE SESSION AS `advise` ASKS ABOUT IT ([`ProcessLogin::me`]): the row its
/// login names, byte for byte, and the two facts carried over. NEGATIVE
/// CONTROLS: a near miss of a row's `dir` is no row, and no roster is none.
#[test]
fn a_process_login_names_its_row_byte_for_byte() {
    let r = three();
    assert_eq!(stored().me(&r), me("main"));
    assert_eq!(
        env_of(&[(LOGIN_ENV, "/logins/work-2")]).me(&r),
        me("work-2")
    );
    assert_eq!(env_of(&[(LOGIN_ENV, "")]).me(&r), me("main"));
    for unlisted in ["/logins/work-2/", "/logins/WORK-2", "/logins/nobody"] {
        assert_eq!(
            env_of(&[(LOGIN_ENV, unlisted)]).me(&r).account,
            None,
            "{unlisted:?}"
        );
    }
    assert_eq!(stored().me(&Roster::default()).account, None);
    let other = env_of(&[
        (LOGIN_ENV, "/logins/pool-a"),
        (HOME_ENV, "/elsewhere"),
        ("ANTHROPIC_BASE_URL", "https://gateway.invalid"),
    ])
    .me(&r);
    assert_eq!(
        other,
        Me {
            account: Some("pool-a"),
            left: None,
            default_home: false,
            stored_login: false,
        }
    );
}

/// [`judged_label`]: the row a process's login names, only for a stored
/// login in the default home. NEGATIVE CONTROLS: a login no row names (a
/// trailing slash is another login), a home of its own, a credential in the
/// environment, and the login variable given twice with two values each name
/// no account — where the row alone ([`ProcessLogin::me`]) still would.
#[test]
fn a_label_is_judged_only_for_a_stored_login_in_the_default_home() {
    let r = three();
    let judged = |env: &[(&str, &str)]| judged_label(&r, &env_of(env));
    // The default login names the row with no `dir`; a login names its row
    // byte for byte.
    assert_eq!(judged(&[]), Some("main"));
    assert_eq!(judged(&[(LOGIN_ENV, "")]), Some("main"));
    assert_eq!(judged(&[(LOGIN_ENV, "/logins/work-2")]), Some("work-2"));
    assert_eq!(judged(&[(LOGIN_ENV, "/logins/pool-a")]), Some("pool-a"));
    assert_eq!(
        judged(&[("HOME", "/home/someone"), ("PATH", "/usr/bin")]),
        Some("main"),
        "what is not the login's says nothing"
    );

    // NEGATIVE CONTROL: a login no row names.
    for unlisted in ["/logins/work-2/", "/logins/WORK-2", "/logins/nobody"] {
        assert_eq!(judged(&[(LOGIN_ENV, unlisted)]), None, "{unlisted:?}");
    }
    assert_eq!(judged_label(&Roster::default(), &stored()), None);
    let no_default = roster(&[("work-2", Class::Dedicated, Some("/logins/work-2"))]);
    assert_eq!(
        judged_label(&no_default, &stored()),
        None,
        "the default login, where no row is without a `dir`"
    );

    // NEGATIVE CONTROLS: each names a row, and is judged as no account.
    let not_judged: [&[(&str, &str)]; 6] = [
        // A home of its own.
        &[(HOME_ENV, "/elsewhere")],
        &[(HOME_ENV, "/elsewhere"), (LOGIN_ENV, "/logins/work-2")],
        // A credential, and a route, in the environment.
        &[("ANTHROPIC_API_KEY", "k")],
        &[
            (LOGIN_ENV, "/logins/work-2"),
            ("ANTHROPIC_BASE_URL", "https://gateway.invalid"),
        ],
        // The login variable twice, with two values.
        &[(LOGIN_ENV, "/logins/work-2"), (LOGIN_ENV, "/logins/pool-a")],
        &[(LOGIN_ENV, "/logins/work-2"), (LOGIN_ENV, "")],
    ];
    for env in not_judged {
        let who = env_of(env);
        assert!(
            who.me(&r).account.is_some(),
            "control: a row is named: {env:?}"
        );
        assert_eq!(judged_label(&r, &who), None, "{env:?}");
    }
    // …and the same value twice is one login.
    assert_eq!(
        judged(&[(LOGIN_ENV, "/logins/work-2"), (LOGIN_ENV, "/logins/work-2")]),
        Some("work-2")
    );

    // It judges what it is handed: the host's half takes the label away too.
    assert_eq!(judged_label(&r, &stored().with_profile_store(true)), None);
}

/// [`ProcessLogin::of_exec_env`]: an environment that reads EMPTY was not
/// read, and names no login — never the default one. NEGATIVE CONTROL: one
/// that holds entries and none of the login's IS the default login. An
/// entry is cut at its first `=`, the rest being the value's.
#[test]
fn an_exec_environment_that_reads_empty_names_no_login() {
    let of = |entries: &[&str]| {
        let env: Vec<String> = entries.iter().map(|kv| (*kv).to_owned()).collect();
        ProcessLogin::of_exec_env(&env)
    };
    assert_eq!(of(&[]), None);
    assert_eq!(of(&["HOME=/home/someone", "PATH=/usr/bin"]), Some(stored()));
    let login = format!("{LOGIN_ENV}=/logins/a=b");
    assert_eq!(
        of(&["HOME=/home/someone", &login]).and_then(|who| who.login),
        Some("/logins/a=b".to_owned())
    );
    let home = format!("{HOME_ENV}=/elsewhere");
    assert_eq!(of(&[&home]).map(|who| who.default_home), Some(false));
}

/// [`footer_label`]: [`judged_label`], where the roster lists two or more.
/// NEGATIVE CONTROL: the same login under a roster of one row is judged, and
/// no surface names it; what is not judged is never shown.
#[test]
fn the_footer_names_an_account_only_where_two_are_listed() {
    let two = roster(&[
        ("main", Class::Dedicated, None),
        ("work-2", Class::Dedicated, Some("/logins/work-2")),
    ]);
    let on_work = env_of(&[(LOGIN_ENV, "/logins/work-2")]);
    assert_eq!(footer_label(&two, &stored()), Some("main"));
    assert_eq!(footer_label(&two, &on_work), Some("work-2"));
    assert_eq!(footer_label(&three(), &on_work), Some("work-2"));

    // NEGATIVE CONTROL: one row is one login, whichever row it is.
    for one in [
        roster(&[("main", Class::Dedicated, None)]),
        roster(&[("work-2", Class::Dedicated, Some("/logins/work-2"))]),
    ] {
        let who = if one.rows[0].dir.is_none() {
            stored()
        } else {
            on_work.clone()
        };
        assert!(judged_label(&one, &who).is_some(), "control: it is judged");
        assert_eq!(footer_label(&one, &who), None, "{one:?}");
    }
    assert_eq!(footer_label(&Roster::default(), &stored()), None);

    // What is not judged is not shown, however many are listed.
    assert_eq!(
        footer_label(&two, &env_of(&[(LOGIN_ENV, "/logins/work-2/")])),
        None
    );
    assert_eq!(
        footer_label(&two, &env_of(&[(HOME_ENV, "/elsewhere")])),
        None
    );
    assert_eq!(
        footer_label(&two, &env_of(&[("ANTHROPIC_API_KEY", "k")])),
        None
    );
}

#[test]
fn the_roster_is_beside_the_config_by_one_rule() {
    use std::path::{Path, PathBuf};
    assert_eq!(
        roster_beside(Path::new("/etc/aterm/aterm.toml")),
        Some(PathBuf::from("/etc/aterm/accounts.toml"))
    );
    assert_eq!(
        roster_beside(Path::new("/elsewhere/custom.toml")),
        Some(PathBuf::from("/elsewhere/accounts.toml")),
        "beside the file, whatever it is called"
    );
    assert_eq!(
        roster_beside(Path::new("aterm.toml")),
        Some(PathBuf::from("accounts.toml"))
    );
    // NEGATIVE CONTROL: a path with no directory to be beside has none.
    assert_eq!(roster_beside(Path::new("/")), None);
    assert_eq!(roster_beside(Path::new("")), None);
}

/// A weekly wall written ten seconds ago, resetting in an hour.
fn weekly() -> StandingWall {
    StandingWall {
        at: (NOW - 10).unsigned_abs(),
        until: Some((NOW + HOUR).unsigned_abs()),
        window: Some("seven_day".to_string()),
        credits: None,
    }
}

/// The process that wrote [`weekly`]'s row started a minute before it.
const STARTED: i64 = NOW - 70;

#[cfg(unix)]
fn inode(path: &std::path::Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).expect("stat").ino()
}

#[test]
fn the_recorder_records_this_process_wall_for_its_account_once() {
    let dir = tmp("recorder");
    let store = Store::at(dir.path());
    let r = three();
    let recorded = record_wall(&store, &r, "s-one", &stored(), &weekly(), STARTED, NOW);
    assert_eq!(
        recorded,
        Recorded::Wall {
            account: "main".to_string(),
            window: "seven_day".to_string(),
            until: Some(NOW + HOUR),
            events: 1,
        }
    );

    // On record: the wall under the vendor's limit type, stamped with this
    // machine's id, and the session on the account.
    let (state, note) = store.load(NOW).expect("ours");
    assert_eq!(note, None);
    assert_eq!(
        state.accounts["main"].walls["seven_day"],
        Wall {
            until: Some(NOW + HOUR),
            at: NOW - 10,
            by: state.machine.clone(),
            attempts: 0,
            cleared: false,
        }
    );
    assert_eq!(
        state.sessions["s-one"],
        SessionRow {
            account: "main".to_string(),
            at: NOW,
            held: None,
        }
    );
    assert_eq!(
        cond(&r, &state, "main", NOW),
        Condition::Limited {
            until: NOW + HOUR + RESET_GRACE_S
        }
    );
    // The ledger's `wall` row carries the session.
    let rows = store.tail(10).expect("reads");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (
            rows[0].kind,
            rows[0].account.as_str(),
            rows[0].sid.as_deref(),
            rows[0].t
        ),
        (Kind::Wall, "main", Some("s-one"), NOW - 10)
    );
    assert!(!store.wake_path().exists(), "a wall wakes nobody");
    #[cfg(unix)]
    assert_eq!(mode(&store.state_file()), 0o600);

    // A SECOND CALL IS UNCHANGED: nothing is written, at this look or at
    // one an hour of looks later.
    let bytes = std::fs::read(store.state_file()).expect("reads");
    #[cfg(unix)]
    let file = inode(&store.state_file());
    for later in [NOW, NOW + 5, NOW + 20 * MIN] {
        let again = record_wall(&store, &r, "s-one", &stored(), &weekly(), STARTED, later);
        assert_eq!(again, Recorded::Unchanged, "{later}");
    }
    assert_eq!(std::fs::read(store.state_file()).expect("reads"), bytes);
    #[cfg(unix)]
    assert_eq!(
        inode(&store.state_file()),
        file,
        "the file was not replaced"
    );
    assert_eq!(store.tail(10).expect("reads").len(), 1);

    // A SECOND SESSION AT THE SAME WALL writes its own session row and no
    // second `wall` row.
    let second = record_wall(&store, &r, "s-two", &stored(), &weekly(), STARTED, NOW + 5);
    assert_eq!(
        second,
        Recorded::Wall {
            account: "main".to_string(),
            window: "seven_day".to_string(),
            until: Some(NOW + HOUR),
            events: 0,
        }
    );
    let (state, _) = store.load(NOW + 5).expect("ours");
    assert_eq!(state.sessions_on("main"), ["s-one", "s-two"]);
    assert_eq!(state.accounts["main"].walls.len(), 1);
    assert_eq!(store.tail(10).expect("reads").len(), 1);

    // ANOTHER LOGIN'S PROCESS: the wall is that account's, by its own
    // environment — and the session, last read on `main`, is a `move`.
    let work = env_of(&[(LOGIN_ENV, "/logins/work-2")]);
    let session = StandingWall {
        at: (NOW + 30).unsigned_abs(),
        until: Some((NOW + 2 * HOUR).unsigned_abs()),
        window: Some("five_hour".to_string()),
        credits: None,
    };
    let moved = record_wall(&store, &r, "s-two", &work, &session, NOW + 20, NOW + 40);
    assert_eq!(
        moved,
        Recorded::Wall {
            account: "work-2".to_string(),
            window: "five_hour".to_string(),
            until: Some(NOW + 2 * HOUR),
            events: 2,
        }
    );
    let rows = store.tail(10).expect("reads");
    assert_eq!(
        rows.iter()
            .map(|e| (
                e.kind,
                e.account.as_str(),
                e.to.as_deref(),
                e.sid.as_deref()
            ))
            .collect::<Vec<_>>(),
        [
            (Kind::Wall, "main", None, Some("s-one")),
            (Kind::Move, "main", Some("work-2"), Some("s-two")),
            (Kind::Wall, "work-2", None, Some("s-two")),
        ]
    );
    let (state, _) = store.load(NOW + 40).expect("ours");
    assert_eq!(state.sessions_on("work-2"), ["s-two"]);
    assert_eq!(state.accounts["work-2"].walls["five_hour"].at, NOW + 30);
    assert_eq!(state.accounts["main"].walls.len(), 1, "main's is untouched");
}

#[test]
fn the_recorder_records_nothing_for_a_session_the_roster_does_not_govern() {
    let dir = tmp("recorder-not");
    let store = Store::at(dir.path());
    let r = three();
    let record = |r: &Roster, who: &ProcessLogin| {
        record_wall(&store, r, "s-one", who, &weekly(), STARTED, NOW)
    };

    // NO ROSTER, and a login no row names.
    assert_eq!(record(&Roster::default(), &stored()), Recorded::NotListed);
    for login in ["/logins/nobody", "/logins/work-2/"] {
        assert_eq!(
            record(&r, &env_of(&[(LOGIN_ENV, login)])),
            Recorded::NotListed,
            "{login}"
        );
    }
    // NOT THE DEFAULT HOME, and not a stored login (its own environment's
    // word, and the host's).
    for who in [
        env_of(&[(HOME_ENV, "/elsewhere")]),
        env_of(&[("ANTHROPIC_API_KEY", "k")]),
        env_of(&[("CLAUDE_CODE_USE_BEDROCK", "1")]),
        env_of(&[
            (LOGIN_ENV, "/logins/work-2"),
            ("ANTHROPIC_BASE_URL", "https://gateway.invalid"),
        ]),
        stored().with_profile_store(true),
    ] {
        assert_eq!(record(&r, &who), Recorded::NotStored, "{who:?}");
    }
    // A roster with no row for the login is said before the home is: there
    // is no account to speak of.
    let unlisted_elsewhere = env_of(&[(LOGIN_ENV, "/logins/nobody"), (HOME_ENV, "/elsewhere")]);
    assert_eq!(record(&r, &unlisted_elsewhere), Recorded::NotListed);

    // NOTHING WAS READ, MADE OR LOCKED for any of them: with no roster the
    // feature leaves no trace on the machine.
    assert!(!store.dir.exists(), "no directory, no lock, no state");

    // NEGATIVE CONTROL: the same call for a governed session records.
    assert!(matches!(record(&r, &stored()), Recorded::Wall { .. }));
    assert!(store.state_file().exists());
}

/// THE ROW OLDER THAN THE PROCESS IS RECORDED FOR NOBODY (design §5). After
/// a relaunch under another login the transcript's last limit row is still
/// the PREVIOUS process's: read as this process's, the wall of the account
/// that was left would be recorded on the account moved to — the fresh one,
/// which would then be skipped as limited.
#[test]
fn a_wall_row_older_than_the_process_is_the_previous_accounts_and_is_recorded_for_nobody() {
    let dir = tmp("recorder-older");
    let store = Store::at(dir.path());
    let r = three();
    let row = weekly();
    let written = NOW - 10;
    // The session was moved to work-2 and its process started after the row.
    let work = env_of(&[(LOGIN_ENV, "/logins/work-2")]);
    for started in [written + 1, written + 30, NOW, NOW + DAY] {
        let recorded = record_wall(&store, &r, "s-one", &work, &row, started, NOW);
        assert_eq!(recorded, Recorded::NotThisProcess, "{started}");
    }
    assert!(!store.dir.exists(), "nothing was read, made or locked");

    // NEGATIVE CONTROL: the same row, the process older than it, IS
    // recorded — and so is a row written in the process's first second: a
    // process cannot write a row before it starts.
    for (sid, started) in [
        ("s-one", written - 1),
        ("s-two", written),
        ("s-three", i64::MIN),
    ] {
        let recorded = record_wall(&store, &r, sid, &work, &row, started, NOW);
        assert!(
            matches!(recorded, Recorded::Wall { .. }),
            "{sid}: {recorded:?}"
        );
    }
    let (state, _) = store.load(NOW).expect("ours");
    assert_eq!(state.sessions_on("work-2"), ["s-one", "s-three", "s-two"]);
    assert_eq!(state.accounts["work-2"].walls["seven_day"].at, written);
    assert!(!state.accounts.contains_key("main"));
}

/// THE WALL IS THE ACCOUNT'S OF THE PROCESS'S OWN LOGIN, WHATEVER ACCOUNT IS
/// ACTIVE (design §5) — a session not moved yet, or started by hand, runs on
/// the login of its own environment — AND THE RECORDER RECORDS, IT DOES NOT
/// STRIKE: a wall on the account that has not answered yet leaves `active`,
/// its proof, the canary and what the rotation wrote over where they were.
#[test]
fn a_wall_is_recorded_for_the_process_login_whatever_is_active_and_strikes_nothing() {
    let r = three();
    let work = env_of(&[(LOGIN_ENV, "/logins/work-2")]);
    let stands = Condition::Limited {
        until: NOW + HOUR + RESET_GRACE_S,
    };

    // `main` is the proven active account, and a session still running under
    // work-2's login hits a wall: it is work-2's, and nothing of main's.
    let dir = tmp("recorder-not-the-active");
    let store = Store::at(dir.path());
    let seeded = on_main();
    store.save(&seeded).expect("saves");
    let recorded = record_wall(&store, &r, "s-two", &work, &weekly(), STARTED, NOW);
    assert_eq!(
        recorded,
        Recorded::Wall {
            account: "work-2".to_string(),
            window: "seven_day".to_string(),
            until: Some(NOW + HOUR),
            events: 1,
        }
    );
    let (after, _) = store.load(NOW).expect("ours");
    assert_eq!(cond(&r, &after, "work-2", NOW), stands);
    assert!(!after.accounts.contains_key("main"), "nothing of main's");
    assert_eq!(cond(&r, &after, "main", NOW), Condition::Unseen);
    assert_eq!(after.sessions_on("work-2"), ["s-two"]);
    assert_eq!(
        State {
            accounts: seeded.accounts.clone(),
            sessions: seeded.sessions.clone(),
            ..after.clone()
        },
        seeded,
        "the machine's own facts are as they were"
    );
    // IT DOES NOT STRIKE. `s-one` rotated to work-2, which has not answered,
    // and meets work-2's wall: the wall is on record and the rotation stands.
    let dir = tmp("recorder-no-strike");
    let store = Store::at(dir.path());
    let mut rotated = on_main();
    limited(&mut rotated, "main", NOW + DAY);
    rotation(&mut rotated, Some("main"), "main", "work-2", NOW).expect("rotates");
    store.save(&rotated).expect("saves");
    let recorded = record_wall(&store, &r, "s-one", &work, &weekly(), STARTED, NOW);
    assert!(matches!(recorded, Recorded::Wall { .. }), "{recorded:?}");
    let (after, _) = store.load(NOW).expect("ours");
    assert_eq!(cond(&r, &after, "work-2", NOW), stands);
    assert_eq!(
        (after.active.as_deref(), after.proven, after.undo.len()),
        (Some("work-2"), false, 1)
    );
    assert_eq!(
        State {
            accounts: rotated.accounts.clone(),
            sessions: rotated.sessions.clone(),
            ..after.clone()
        },
        rotated,
        "`active`, its proof, `previous`, the canary, the undo, the rotation's time"
    );
    assert_eq!(store.tail(10).expect("reads").len(), 1, "one `wall` row");
    assert!(!store.wake_path().exists(), "and nobody is woken");
    // NEGATIVE CONTROL: the strike is what puts `active` back.
    let mut struck = after.clone();
    let word = Strike::Wall {
        window: "seven_day".into(),
        until: Some(NOW + HOUR),
    };
    assert_eq!(
        strike(&mut struck, "work-2", "s-one", &word, NOW)[0].kind,
        Kind::Struck
    );
    assert_eq!(
        (struck.active.as_deref(), struck.proven, struck.undo.len()),
        (Some("main"), true, 0)
    );
}

#[test]
fn the_recorder_reads_a_row_of_any_shape_into_a_wall_the_state_can_hold() {
    let r = three();
    let record = |tag: &str, sid: &str, wall: &StandingWall, now: i64| {
        let dir = tmp(tag);
        let store = Store::at(dir.path());
        let recorded = record_wall(&store, &r, sid, &stored(), wall, STARTED, now);
        let (state, _) = store.load(now).expect("ours");
        let rows = store.tail(10).expect("reads");
        (recorded, state, rows)
    };

    // NO LIMIT TYPE: recorded under the one unnamed window, and so is one
    // that is empty once it is folded onto a line.
    for window in [None, Some(String::new()), Some(" \n\t".to_string())] {
        let unnamed = StandingWall {
            window: window.clone(),
            ..weekly()
        };
        let (recorded, state, _) = record("recorder-unnamed", "s-one", &unnamed, NOW);
        assert_eq!(
            recorded,
            Recorded::Wall {
                account: "main".to_string(),
                window: UNNAMED_WINDOW.to_string(),
                until: Some(NOW + HOUR),
                events: 1,
            },
            "{window:?}"
        );
        assert!(state.accounts["main"].walls.contains_key("limit"));
    }
    // NO RESET: the wall stands on the ladder of one that names none.
    let no_reset = StandingWall {
        until: None,
        ..weekly()
    };
    let (recorded, state, _) = record("recorder-no-reset", "s-one", &no_reset, NOW);
    assert!(matches!(recorded, Recorded::Wall { .. }));
    assert_eq!(
        cond(&r, &state, "main", NOW),
        Condition::Limited {
            until: NOW - 10 + NO_RESET_WALL_S[0]
        }
    );
    // A reset past what the state's clock holds is no reset.
    let absurd = StandingWall {
        until: Some(u64::MAX),
        ..weekly()
    };
    let (recorded, state, _) = record("recorder-absurd", "s-one", &absurd, NOW);
    assert_eq!(
        recorded,
        Recorded::Wall {
            account: "main".to_string(),
            window: "seven_day".to_string(),
            until: None,
            events: 1,
        }
    );
    assert_eq!(state.accounts["main"].walls["seven_day"].until, None);

    // A ROW STAMPED AHEAD OF THE CLOCK is recorded as written now: a wall
    // observed "later" than now would stand past its own reset's grace for
    // as long as the clock is behind it.
    for ahead in [(NOW + 3 * DAY).unsigned_abs(), u64::MAX] {
        let future = StandingWall {
            at: ahead,
            ..weekly()
        };
        let (recorded, state, rows) = record("recorder-ahead", "s-one", &future, NOW);
        assert!(matches!(recorded, Recorded::Wall { .. }), "{ahead}");
        assert_eq!(state.accounts["main"].walls["seven_day"].at, NOW, "{ahead}");
        assert_eq!(rows[0].t, NOW);
        assert_eq!(
            cond(&r, &state, "main", NOW),
            Condition::Limited {
                until: NOW + HOUR + RESET_GRACE_S
            },
            "its reset is ahead of it, so it stands on that"
        );
    }

    // A SESSION WITH NO ID is still a process on that account: the wall is
    // recorded, and no session row is.
    let (recorded, state, rows) = record("recorder-no-sid", " \n", &weekly(), NOW);
    assert!(matches!(recorded, Recorded::Wall { .. }));
    assert!(state.sessions.is_empty());
    assert_eq!(
        (rows.len(), rows[0].kind, rows[0].sid.as_deref()),
        (1, Kind::Wall, None)
    );
    // An id is cut to the bound the state keeps, and the ledger says the
    // same one.
    let long = "s".repeat(WORD_MAX + 40);
    let (_, state, rows) = record("recorder-long-sid", &long, &weekly(), NOW);
    let kept: Vec<&String> = state.sessions.keys().collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].len(), WORD_MAX, "{}", kept[0]);
    assert_eq!(rows[0].sid.as_ref(), Some(kept[0]));
}

#[test]
fn the_recorder_keeps_what_is_on_record_and_ends_a_pin_at_its_wall() {
    let dir = tmp("recorder-pin");
    let store = Store::at(dir.path());
    let r = three();
    let enrolled = all(&r);

    // The owner pinned `main`, and a session there could not move (`held`).
    let mut seeded = state();
    pin(&mut seeded, &r, &enrolled, "main", NOW - HOUR).expect("pins");
    note_session(&mut seeded, "s-one", "main", Some("jobs"), NOW - HOUR);
    low(&mut seeded, "main");
    store.save(&seeded).expect("saves");

    // A WALL ON THE PINNED ACCOUNT ENDS THE PIN: both rows carry the
    // session, and the wake marker is replaced (a waiting tab decides again).
    let recorded = record_wall(&store, &r, "s-one", &stored(), &weekly(), STARTED, NOW);
    assert_eq!(
        recorded,
        Recorded::Wall {
            account: "main".to_string(),
            window: "seven_day".to_string(),
            until: Some(NOW + HOUR),
            events: 2,
        }
    );
    let rows = store.tail(10).expect("reads");
    assert_eq!(
        rows.iter()
            .map(|e| (e.kind, e.sid.as_deref()))
            .collect::<Vec<_>>(),
        [(Kind::Wall, Some("s-one")), (Kind::Unpin, Some("s-one"))]
    );
    assert!(store.wake_path().exists(), "the pin's end wakes");
    let (after, _) = store.load(NOW).expect("ours");
    assert!(!after.pinned);
    // EVERYTHING ELSE ON RECORD IS KEPT: the machine's id, the sample, the
    // active account and its proof — and the session's row as it was, its
    // `held` word included (the recorder says nothing of a refused move).
    assert_eq!(after.sessions["s-one"], seeded.sessions["s-one"]);
    assert_eq!(
        State {
            pinned: true,
            accounts: seeded.accounts.clone(),
            ..after.clone()
        },
        seeded
    );
    assert_eq!(
        after.accounts["main"].sample,
        seeded.accounts["main"].sample
    );
    assert_eq!(after.accounts["main"].walls.len(), 1);

    // NEGATIVE CONTROL: with no pin the same wall is one row and no wake.
    let dir = tmp("recorder-no-pin");
    let store = Store::at(dir.path());
    let recorded = record_wall(&store, &r, "s-one", &stored(), &weekly(), STARTED, NOW);
    assert!(matches!(recorded, Recorded::Wall { .. }));
    assert_eq!(store.tail(10).expect("reads").len(), 1);
    assert!(!store.wake_path().exists());
}

/// THE OWNER'S CLEARING STANDS AGAINST A ROW STILL IN A TRANSCRIPT (design
/// §6, "The owner's word"). `accounts use` clears the account's walls and
/// pins it; a session left standing at that wall has taken no turn, so its
/// transcript's last limit row is still the wall the owner cleared, and the
/// host hands it to the recorder at every look.
///
/// Read as a wall met now, the first look after the owner's word put the
/// wall back and ended the pin (`unpin: wall`): the owner's word lasted
/// until the next look.
#[test]
fn a_wall_the_owner_cleared_is_not_put_back_by_its_row_and_ends_no_pin() {
    let r = three();
    let enrolled = all(&r);
    let dir = tmp("recorder-cleared");
    let store = Store::at(dir.path());
    // A session stands at main's weekly wall, and the wall is on record.
    let recorded = record_wall(&store, &r, "s-one", &stored(), &weekly(), STARTED, NOW);
    assert!(matches!(recorded, Recorded::Wall { .. }));
    // The owner's word, a minute on.
    let used = NOW + MIN;
    let mut note = None;
    store
        .transact(used, &mut note, |state| {
            pin(state, &r, &enrolled, "main", used).map(|event| vec![event])
        })
        .expect("pins");
    let (pinned, _) = store.load(used).expect("ours");
    assert!(pinned.pinned && pinned.accounts["main"].walls.is_empty());
    let rows = store.tail(10).expect("reads").len();

    // THE SAME ROW, at the next look and at every later one, from that
    // session and from one not on record yet: no wall, no row, and the pin
    // holds.
    for (sid, later) in [
        ("s-one", used + 5),
        ("s-two", used + 10),
        ("s-one", used + HOUR),
    ] {
        let again = record_wall(&store, &r, sid, &stored(), &weekly(), STARTED, later);
        assert_eq!(again, Recorded::Cleared, "{sid} at {later}");
        let (after, _) = store.load(later).expect("ours");
        assert!(after.pinned, "{sid} at {later}");
        assert!(after.accounts["main"].walls.is_empty(), "{sid} at {later}");
        assert!(cond(&r, &after, "main", later).runs());
    }
    assert_eq!(store.tail(10).expect("reads").len(), rows, "no ledger row");
    // The session is on its account all the same.
    let (after, _) = store.load(used + HOUR).expect("ours");
    assert_eq!(after.sessions_on("main"), ["s-one", "s-two"]);

    // NEGATIVE CONTROL: a wall the session meets AFTER the owner's word is a
    // new row, and it is recorded and ends the pin.
    let met = StandingWall {
        at: (used + 2 * HOUR).unsigned_abs(),
        until: Some((NOW + DAY).unsigned_abs()),
        ..weekly()
    };
    let recorded = record_wall(
        &store,
        &r,
        "s-one",
        &stored(),
        &met,
        STARTED,
        used + 2 * HOUR + 5,
    );
    assert_eq!(
        recorded,
        Recorded::Wall {
            account: "main".to_string(),
            window: "seven_day".to_string(),
            until: Some(NOW + DAY),
            events: 2,
        }
    );
    let (after, _) = store.load(used + 2 * HOUR + 5).expect("ours");
    assert!(!after.pinned);
    assert_eq!(
        after.accounts["main"].walls["seven_day"].at,
        used + 2 * HOUR
    );

    // THE RULE IS `note_wall`'s OWN, whoever hands it an old observation.
    let mut s = on_main();
    pin(&mut s, &r, &enrolled, "main", NOW).expect("pins");
    let until = Some(NOW + HOUR);
    let before = s.clone();
    assert!(note_wall(&mut s, "main", "five_hour", until, NOW - 1, NOW + 5).is_empty());
    assert_eq!(s, before, "nothing of the state moved");
    // An observation of the clearing's own second is read as made after it…
    let mut tied = s.clone();
    let kinds = |events: Vec<Event>| events.iter().map(|e| e.kind).collect::<Vec<_>>();
    assert_eq!(
        kinds(note_wall(
            &mut tied,
            "main",
            "five_hour",
            until,
            NOW,
            NOW + 5
        )),
        [Kind::Wall, Kind::Unpin]
    );
    // …a clearing stamped ahead of the clock is read as made now, so a wall
    // met now is not shadowed until the clock catches up…
    let mut ahead = s.clone();
    ahead.accounts.get_mut("main").expect("cleared").cleared_at = Some(NOW + DAY);
    assert_eq!(
        kinds(note_wall(
            &mut ahead,
            "main",
            "five_hour",
            until,
            NOW + 5,
            NOW + 5
        )),
        [Kind::Wall, Kind::Unpin]
    );
    // …and another account's walls are not the ones cleared.
    assert_eq!(
        kinds(note_wall(
            &mut s,
            "work-2",
            "five_hour",
            until,
            NOW - 1,
            NOW + 5
        )),
        [Kind::Wall]
    );
    assert!(s.pinned);
}

#[test]
fn the_recorder_refuses_a_state_that_is_not_ours_and_leaves_it_byte_for_byte() {
    let r = three();
    // Which files are not ours is the store's test's; one is enough here.
    let not_ours = r#"{"schema":2,"accounts":{"main":{"auth":"signed-out"}},"pinned":true}"#;
    let dir = tmp("recorder-refused");
    let store = Store::at(dir.path());
    store.ensure().expect("dir");
    std::fs::write(store.state_file(), not_ours).expect("writes");
    let recorded = record_wall(&store, &r, "s-one", &stored(), &weekly(), STARTED, NOW);
    let Recorded::Refused(why) = &recorded else {
        panic!("{recorded:?}");
    };
    assert!(why.contains("state.json"), "{why}");
    let unchanged = || std::fs::read_to_string(store.state_file()).expect("reads");
    assert_eq!(unchanged(), not_ours, "byte for byte");
    assert!(
        !store.ledger_path().exists(),
        "no row of a wall not recorded"
    );
    assert!(!store.wake_path().exists());
    // …and the same for a session that ended.
    let gone = record_gone(&store, "s-one", NOW);
    assert!(gone.is_err(), "{gone:?}");
    assert_eq!(unchanged(), not_ours);

    // NEGATIVE CONTROL: a TORN file holds nothing to lose, and is replaced
    // by the state with the wall.
    let dir = tmp("recorder-torn");
    let store = Store::at(dir.path());
    store.ensure().expect("dir");
    std::fs::write(store.state_file(), r#"{"schema":1,"accou"#).expect("writes");
    let recorded = record_wall(&store, &r, "s-one", &stored(), &weekly(), STARTED, NOW);
    assert!(matches!(recorded, Recorded::Wall { .. }), "{recorded:?}");
    let (state, note) = store.load(NOW).expect("ours now");
    assert_eq!(note, None);
    assert!(state.accounts["main"].walls.contains_key("seven_day"));
}

/// A STORE WHOSE DIRECTORY CANNOT BE MADE (a file stands where its parent
/// should be) is said as `refused`, by the directory's name: no panic, and
/// nothing written anywhere else. A session's end has nothing to forget
/// there. NEGATIVE CONTROL: the same call where the directory can be made
/// records.
#[test]
fn the_recorder_says_refused_where_its_directory_cannot_be_made() {
    let dir = tmp("recorder-no-dir");
    let r = three();
    let file = dir.path().join("a-file");
    std::fs::write(&file, "x").expect("writes");
    let store = Store::at(&file);
    let recorded = record_wall(&store, &r, "s-one", &stored(), &weekly(), STARTED, NOW);
    let Recorded::Refused(why) = &recorded else {
        panic!("{recorded:?}");
    };
    assert!(why.contains(&store.dir.display().to_string()), "{why}");
    assert_eq!(std::fs::read_to_string(&file).expect("reads"), "x");
    assert_eq!(record_gone(&store, "s-one", NOW), Ok(false));
    assert_eq!(std::fs::read_dir(dir.path()).expect("lists").count(), 1);

    let store = Store::at(&dir.path().join("a-dir"));
    let recorded = record_wall(&store, &r, "s-one", &stored(), &weekly(), STARTED, NOW);
    assert!(matches!(recorded, Recorded::Wall { .. }), "{recorded:?}");
}

#[test]
fn a_session_that_ended_is_forgotten_and_a_canarys_end_wakes_the_others() {
    let r = three();

    // A MACHINE WITH NO STATE has nothing to forget, and nothing is made.
    let dir = tmp("gone-none");
    let store = Store::at(dir.path());
    assert_eq!(record_gone(&store, "s-one", NOW), Ok(false));
    assert!(!store.dir.exists(), "no directory, no lock");

    // A session on record is forgotten; one that is not changes nothing,
    // and the file is not replaced for it.
    let dir = tmp("gone");
    let store = Store::at(dir.path());
    for sid in ["s-one", "s-two"] {
        let recorded = record_wall(&store, &r, sid, &stored(), &weekly(), STARTED, NOW);
        assert!(matches!(recorded, Recorded::Wall { .. }));
    }
    assert_eq!(record_gone(&store, "s-one", NOW + 5), Ok(true));
    let (state, _) = store.load(NOW + 5).expect("ours");
    assert_eq!(state.sessions_on("main"), ["s-two"]);
    assert!(
        state.accounts["main"].walls.contains_key("seven_day"),
        "the wall is the account's, and stays"
    );
    #[cfg(unix)]
    let file = inode(&store.state_file());
    for nobody in ["s-one", "s-nine", ""] {
        assert_eq!(
            record_gone(&store, nobody, NOW + 6),
            Ok(false),
            "{nobody:?}"
        );
    }
    #[cfg(unix)]
    assert_eq!(inode(&store.state_file()), file, "nothing was written");
    assert!(!store.wake_path().exists(), "no hold ended");
    assert_eq!(store.tail(10).expect("reads").len(), 1, "no ledger row");

    // THE CANARY'S AGENT ENDS: its hold ends at once and the wake marker is
    // replaced, so a session waiting for the hold decides again.
    let dir = tmp("gone-canary");
    let store = Store::at(dir.path());
    let mut s = on_main();
    limited(&mut s, "main", NOW + DAY);
    note_session(&mut s, "s-two", "main", None, NOW);
    rotation(&mut s, Some("main"), "main", "work-2", NOW).expect("rotates");
    note_session(&mut s, "s-one", "work-2", None, NOW);
    assert!(s.canary_holds(NOW + 5));
    store.save(&s).expect("saves");
    // NEGATIVE CONTROL: another session's end leaves the hold and the
    // marker alone.
    assert_eq!(record_gone(&store, "s-two", NOW + 5), Ok(true));
    assert!(!store.wake_path().exists());
    assert!(store.load(NOW + 5).expect("ours").0.canary_holds(NOW + 5));
    assert_eq!(record_gone(&store, "s-one", NOW + 6), Ok(true));
    assert!(store.wake_path().exists(), "the hold's end wakes");
    let (after, _) = store.load(NOW + 6).expect("ours");
    assert_eq!(
        (after.active.as_deref(), after.proven, &after.canary),
        (Some("work-2"), false, &None),
        "`active` is not put back"
    );
    assert!(!after.canary_holds(NOW + 6));
    assert!(after.sessions.is_empty());
}

/// [`several_listed`]: two or more rows in the roster's file, and only then.
/// NEGATIVE CONTROLS: no path, no file, one row, and a roster that does not
/// admit (two rows and a key nobody knows) list none — everything is read as
/// it was before accounts existed.
#[test]
fn several_accounts_are_listed_only_by_a_roster_that_admits_two() {
    let dir = tmp("several");
    let path = dir.path().join(ACCOUNTS_FILE);
    assert!(!several_listed(None));
    assert!(!several_listed(Some(&path)), "no file");
    let one = "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n";
    std::fs::write(&path, one).unwrap();
    assert!(!several_listed(Some(&path)), "one account is one login");
    let two = format!(
        "{one}\n[[account]]\nlabel = \"work-2\"\nclass = \"dedicated\"\ndir = \"{}\"\n",
        abs("/logins/work-2")
    );
    std::fs::write(&path, &two).unwrap();
    assert!(several_listed(Some(&path)));
    std::fs::write(&path, format!("rotate = true\n{two}")).unwrap();
    assert!(Roster::load(&path).is_err(), "control: it does not admit");
    assert!(!several_listed(Some(&path)), "a refused roster lists none");
}

// ---------------------------------------------------------------------------
// 9. The recorder's hardening
// ---------------------------------------------------------------------------

/// A RESET MORE THAN EIGHT DAYS PAST ITS ROW IS NOT BELIEVED: the longest
/// window is a week, so such a row names no reset this module can stand a
/// wall on. It is recorded as a wall naming none — the ladder of thirty
/// minutes, two hours, six — whoever hands it over: the recorder, the host's
/// commit (`note_wall`) and a strike.
///
/// Believed, the account read `limited` until that instant: one bad row and
/// it was out of the rotation for as long as the row said.
#[test]
fn a_reset_more_than_eight_days_past_its_row_is_recorded_as_naming_none() {
    let r = three();
    let written = NOW - 10;
    let row = |past: i64| StandingWall {
        until: Some((written + past).unsigned_abs()),
        ..weekly()
    };
    assert_eq!(believed_reset(Some(NOW + 8 * DAY + 1), NOW), None);
    assert_eq!(
        believed_reset(Some(NOW + 8 * DAY), NOW),
        Some(NOW + 8 * DAY)
    );
    assert_eq!(
        believed_reset(Some(NOW - DAY), NOW),
        Some(NOW - DAY),
        "a reset behind its row is a wall found standing late, and is kept"
    );
    assert_eq!(believed_reset(None, NOW), None);
    let dir = tmp("recorder-far");
    let store = Store::at(dir.path());
    let far = row(8 * DAY + 1);
    let recorded = record_wall(&store, &r, "s-one", &stored(), &far, STARTED, NOW);
    assert_eq!(
        recorded,
        Recorded::Wall {
            account: "main".to_string(),
            window: "seven_day".to_string(),
            until: None,
            events: 1,
        }
    );
    let (state, _) = store.load(NOW).expect("ours");
    assert_eq!(state.accounts["main"].walls["seven_day"].until, None);
    assert_eq!(
        cond(&r, &state, "main", NOW),
        Condition::Limited {
            until: written + NO_RESET_WALL_S[0]
        }
    );
    // The same row at the next look is the same observation: nothing is
    // written, and the wall does not climb its ladder for being looked at.
    let again = record_wall(&store, &r, "s-one", &stored(), &far, STARTED, NOW + 5);
    assert_eq!(again, Recorded::Unchanged);

    // NEGATIVE CONTROL: eight days to the second is a reset, and stands.
    let dir = tmp("recorder-week");
    let store = Store::at(dir.path());
    let recorded = record_wall(&store, &r, "s-one", &stored(), &row(8 * DAY), STARTED, NOW);
    assert_eq!(
        recorded,
        Recorded::Wall {
            account: "main".to_string(),
            window: "seven_day".to_string(),
            until: Some(written + 8 * DAY),
            events: 1,
        }
    );

    // THE RULE IS `note_wall`'s OWN: the host's commit and a strike hand it
    // the same row, and must not put the instant back.
    let mut s = on_main();
    note_wall(&mut s, "main", "seven_day", Some(NOW + 9 * DAY), NOW, NOW);
    assert_eq!(s.accounts["main"].walls["seven_day"].until, None);
    let strike_wall = Strike::Wall {
        window: "five_hour".into(),
        until: Some(NOW + 400 * DAY),
    };
    strike(&mut s, "work-2", "s-one", &strike_wall, NOW);
    assert_eq!(s.accounts["work-2"].walls["five_hour"].until, None);
    assert_eq!(
        cond(&r, &s, "work-2", NOW),
        Condition::Limited {
            until: NOW + NO_RESET_WALL_S[0]
        }
    );
}

/// A SESSION'S ROW OLDER THAN THIRTY DAYS IS DROPPED WHEN THE STATE IS SAVED:
/// a tab closed with no word (a crash, a machine switched off) leaves its
/// row, and the report's `on:` would name it for ever. A transaction that
/// changes nothing saves nothing, so it drops nothing either; a row stamped
/// ahead of the clock is not old.
#[test]
fn a_session_row_older_than_thirty_days_is_dropped_when_the_state_is_saved() {
    let dir = tmp("sessions-old");
    let store = Store::at(dir.path());
    let mut seeded = state();
    note_session(&mut seeded, "s-old", "main", None, NOW - 30 * DAY - 1);
    note_session(&mut seeded, "s-edge", "main", Some("jobs"), NOW - 30 * DAY);
    note_session(&mut seeded, "s-new", "work-2", None, NOW - HOUR);
    note_session(&mut seeded, "s-ahead", "main", None, NOW + DAY);
    store.save(&seeded).expect("saves");
    assert_eq!(drop_stale_sessions(&mut seeded.clone(), NOW), 1);
    assert_eq!(
        drop_stale_sessions(&mut seeded.clone(), NOW - 1),
        0,
        "a second earlier, the oldest row is thirty days old to the second"
    );
    let keys = |store: &Store| {
        let (state, _) = store.load(NOW).expect("ours");
        state.sessions.keys().cloned().collect::<Vec<_>>()
    };

    // NEGATIVE CONTROL: nothing changed, nothing saved, nothing dropped.
    let mut note = None;
    let rows = store.transact(NOW, &mut note, |_| Ok(Vec::new()));
    assert_eq!(rows, Ok(Vec::new()));
    assert_eq!(keys(&store), ["s-ahead", "s-edge", "s-new", "s-old"]);

    // A transaction that writes the state writes it without the old row.
    store
        .transact(NOW, &mut note, |state| {
            Ok(note_session(state, "s-two", "main", None, NOW))
        })
        .expect("writes");
    assert_eq!(keys(&store), ["s-ahead", "s-edge", "s-new", "s-two"]);
    let (state, _) = store.load(NOW).expect("ours");
    assert_eq!(
        state.sessions["s-edge"].held.as_deref(),
        Some("jobs"),
        "a row kept is kept whole"
    );
}

/// THE VENDOR'S `profile` SOURCE comes ahead of the stored login: a process
/// whose environment names it does not authenticate by a stored login, and
/// neither does one on a machine whose profile store holds a config.
#[test]
fn the_vendors_profile_source_is_not_a_stored_login() {
    use std::path::PathBuf;
    // The names the vendor reads, spelled here and not read off
    // [`PROFILE_KEYS`]: one dropped from the list is a stored login again.
    for name in [
        "ANTHROPIC_PROFILE",
        "ANTHROPIC_FEDERATION_RULE_ID",
        "ANTHROPIC_ORGANIZATION_ID",
        "ANTHROPIC_IDENTITY_TOKEN",
        "ANTHROPIC_IDENTITY_TOKEN_FILE",
        "ANTHROPIC_CONFIG_DIR",
    ] {
        assert!(!env_of(&[(name, "x")]).stored_login, "{name}");
        // NEGATIVE CONTROL: set and empty is not set.
        assert_eq!(env_of(&[(name, "")]), stored(), "{name}=");
    }

    // THE STORE'S PLACE, in the vendor's order: the variable that names it,
    // else the XDG config home, else the home's `.config`. A value of
    // spaces is unset; with none of the three there is no store.
    let dir = |pairs: &[(&str, &str)]| profile_configs_dir(pairs.iter().copied());
    assert_eq!(
        dir(&[("HOME", "/h")]),
        Some(PathBuf::from("/h/.config/anthropic/configs"))
    );
    assert_eq!(
        dir(&[("HOME", "/h"), ("XDG_CONFIG_HOME", "/x")]),
        Some(PathBuf::from("/x/anthropic/configs"))
    );
    assert_eq!(
        dir(&[
            ("XDG_CONFIG_HOME", "/x"),
            ("ANTHROPIC_CONFIG_DIR", "/a"),
            ("HOME", "/h")
        ]),
        Some(PathBuf::from("/a/configs"))
    );
    assert_eq!(
        dir(&[
            ("ANTHROPIC_CONFIG_DIR", " "),
            ("XDG_CONFIG_HOME", ""),
            ("HOME", "/h")
        ]),
        Some(PathBuf::from("/h/.config/anthropic/configs"))
    );
    assert_eq!(dir(&[("PATH", "/usr/bin"), ("HOME", "")]), None);

    // WHETHER IT HOLDS A CONFIG: a `.json` entry, by name alone.
    let store = tmp("profile-store");
    let configs = store.path().join("configs");
    assert!(!profile_configured(&configs), "no such directory");
    std::fs::create_dir(&configs).expect("makes");
    assert!(!profile_configured(&configs), "an empty store");
    std::fs::write(configs.join("notes.txt"), "").expect("writes");
    assert!(!profile_configured(&configs), "nothing that is a config");
    std::fs::write(configs.join("default.json"), "{}").expect("writes");
    assert!(profile_configured(&configs));
    // A store that cannot be listed may hold one.
    assert!(profile_configured(&configs.join("notes.txt")));

    // THE BUILDER only ever takes the stored login away.
    assert_eq!(stored().with_profile_store(false), stored());
    assert!(!stored().with_profile_store(true).stored_login);
    let keyed = env_of(&[("ANTHROPIC_API_KEY", "k")]);
    assert!(!keyed.with_profile_store(false).stored_login);
    let elsewhere = env_of(&[(HOME_ENV, "/elsewhere"), (LOGIN_ENV, "/logins/work-2")]);
    assert_eq!(
        elsewhere.clone().with_profile_store(true),
        ProcessLogin {
            stored_login: false,
            ..elsewhere
        },
        "nothing else is touched"
    );
}

/// THE SETTINGS FILES' HALF (`ProcessLogin::with_settings`, over what the
/// host's gatherer read): a credential, a profile or a route named in an
/// `env` block, a login or a home set there, an `apiKeyHelper`, and a source
/// nobody could read each make it no stored login — so the recorder records
/// its wall for nobody and the advice is to stay.
///
/// Read from the environment alone, a session given another subscription's
/// token by its settings read as on its stored login, and its wall was
/// recorded on that account.
#[test]
fn a_settings_file_can_take_a_stored_login_away_and_never_gives_one() {
    use crate::harness::netwatch::Source;
    let with = |sources: &[Source]| {
        let named: Vec<(String, Source)> = sources
            .iter()
            .map(|source| ("a settings file".to_string(), source.clone()))
            .collect();
        stored().with_settings(&named)
    };
    let block =
        |name: &str, value: &str| Source::Text(format!("{{\"env\":{{\"{name}\":\"{value}\"}}}}"));

    // NEGATIVE CONTROLS: nothing gathered, nothing there, and files that
    // name none of it leave a stored login one.
    assert_eq!(with(&[]), stored());
    assert_eq!(with(&[Source::Missing]), stored());
    let quiet = [
        Source::Text("{}".to_string()),
        Source::Text(
            r#"{"model":"opus","env":{"ANTHROPIC_MODEL":"opus","EDITOR":"vi"}}"#.to_string(),
        ),
        Source::Text(r#"{"apiKeyHelper":null,"env":{}}"#.to_string()),
        Source::Missing,
    ];
    assert_eq!(with(&quiet), stored());

    // Every name the environment is read for, in an `env` block.
    let listed = CREDENTIAL_KEYS
        .iter()
        .chain(PROFILE_KEYS)
        .chain(crate::harness::netwatch::ROUTE_KEYS)
        .chain(&["CLAUDE_CODE_PROXY_URL"]);
    for name in listed {
        assert!(!with(&[block(name, "x")]).stored_login, "{name}");
        assert!(
            !with(&[Source::Missing, quiet[1].clone(), block(name, "0")]).stored_login,
            "{name}: any source of several"
        );
        // NEGATIVE CONTROL: set and empty is not set.
        assert_eq!(with(&[block(name, "")]), stored(), "{name}=");
    }
    // A value that is no string is set all the same.
    let number = Source::Text(r#"{"env":{"CLAUDE_CODE_USE_GATEWAY":1}}"#.to_string());
    assert!(!with(&[number]).stored_login);

    // THE LOGIN AND THE HOME set by a settings file: the process's own
    // environment no longer says which login it is on. Set and empty is a
    // login too (the default one); an empty home is no home.
    assert!(!with(&[block(LOGIN_ENV, "/logins/work-2")]).stored_login);
    assert!(!with(&[block(LOGIN_ENV, "")]).stored_login);
    assert!(!with(&[block(HOME_ENV, "/elsewhere")]).stored_login);
    assert_eq!(with(&[block(HOME_ENV, "")]), stored());

    // AN `apiKeyHelper`, and a source nobody could read or parse.
    let helper = Source::Text(r#"{"apiKeyHelper":"/usr/local/bin/key"}"#.to_string());
    assert!(!with(std::slice::from_ref(&helper)).stored_login);
    assert!(!with(&[Source::Unreadable]).stored_login);
    for unparsed in ["{env", "[1]", "{\"env\":[1]}"] {
        assert!(
            !with(&[Source::Text(unparsed.to_string())]).stored_login,
            "{unparsed}"
        );
    }

    // IT NEVER GIVES ONE, and touches nothing else.
    let keyed = env_of(&[("ANTHROPIC_API_KEY", "k")]);
    assert!(!keyed.with_settings(&[]).stored_login);
    let elsewhere = env_of(&[(HOME_ENV, "/elsewhere"), (LOGIN_ENV, "/logins/work-2")]);
    let named = [("the user's settings".to_string(), helper)];
    assert_eq!(
        elsewhere.clone().with_settings(&named),
        ProcessLogin {
            stored_login: false,
            ..elsewhere
        }
    );
}

/// THE LOCK IS WAITED FOR A BOUNDED TIME by a loop or a host thread
/// (`Store::acquire_within`, `LOCK_LADDER`): held past it, the recorder
/// answers `Refused` with exactly `LOCK_HELD` and a session's end is an
/// error of the same words — nothing read, nothing written — and both
/// record once the lock is free.
///
/// They took the lock with no bound: a holder that was stopped (a suspended
/// `aterm harness accounts`) stopped every session's loop at its wall.
#[test]
fn the_recorder_waits_for_the_lock_a_bounded_time_and_says_it_is_held() {
    use std::time::Duration;
    let r = three();
    let dir = tmp("lock-bounded");
    let store = Store::at(dir.path());
    // A state on record, so a session's end has something to lock for.
    let first = record_wall(&store, &r, "s-one", &stored(), &weekly(), STARTED, NOW);
    assert!(matches!(first, Recorded::Wall { .. }));
    assert!(!first.lock_held());
    let bytes = std::fs::read(store.state_file()).expect("reads");

    let held = store.acquire().expect("the holder");
    // The bounded take: one try and one more after each pause, then none.
    assert!(store.acquire_within(&[]).expect("tries").is_none());
    assert!(
        store
            .acquire_within(&[Duration::ZERO; 3])
            .expect("tries")
            .is_none()
    );
    // A transaction that cannot take it reads nothing and runs no edit.
    let mut ran = false;
    let mut note = None;
    let refused = store.transact_within(&[Duration::ZERO], NOW, &mut note, |_| {
        ran = true;
        Ok(Vec::new())
    });
    assert_eq!(refused, Err(LOCK_HELD.to_string()));
    assert!(!ran);

    // THE RECORDER AND A SESSION'S END, on the product's ladder: each comes
    // back, with the lock's own words. (Two threads: the two waits overlap.)
    let (walled, gone) = std::thread::scope(|scope| {
        let walled = scope
            .spawn(|| record_wall(&store, &r, "s-two", &stored(), &weekly(), STARTED, NOW + 5));
        let gone = scope.spawn(|| record_gone(&store, "s-one", NOW + 5));
        (
            walled.join().expect("the recorder returns"),
            gone.join().expect("the end returns"),
        )
    });
    assert_eq!(walled, Recorded::Refused(LOCK_HELD.to_string()));
    assert!(walled.lock_held());
    assert_eq!(gone, Err(LOCK_HELD.to_string()));
    assert_eq!(std::fs::read(store.state_file()).expect("reads"), bytes);
    assert_eq!(store.tail(10).expect("reads").len(), 1, "no row");

    // NEGATIVE CONTROL: the lock free, the same two calls record.
    drop(held);
    assert!(
        store.acquire_within(&[]).expect("tries").is_some(),
        "free the moment it drops"
    );
    let walled = record_wall(&store, &r, "s-two", &stored(), &weekly(), STARTED, NOW + 5);
    assert!(matches!(walled, Recorded::Wall { .. }));
    assert_eq!(record_gone(&store, "s-one", NOW + 5), Ok(true));
    // Another refusal is not the lock's.
    assert!(!Recorded::Refused("state.json: schema 2".to_string()).lock_held());
}

// ---------------------------------------------------------------------------
// Usage credits and the vendor's warnings (accounts v2 S5)
// ---------------------------------------------------------------------------

/// The machine's own row, as a rosterless machine under `[harness]
/// usage_notes` records it.
fn own_row() -> Roster {
    Roster::own()
}

/// The own row's label.
const OWN: &str = super::super::cli::ACCOUNTS_OWN_LABEL;

/// What a screen says, as true at the answer `at` (on UTC's day).
fn said(at: i64, credits: Option<CreditsRead>, warning: Option<WarningRead>) -> UsageSaid {
    UsageSaid {
        at,
        day: at.div_euclid(DAY),
        credits,
        warning,
    }
}

/// Credits carrying the answer at `at`, past a window resetting at `until`.
fn on_credits(at: i64, until: Option<i64>) -> UsageSaid {
    said(at, Some(CreditsRead { until }), None)
}

/// Claude Code's warning, drawn after the answer at `at`, that the week (or
/// the session window) is at `pct`, resetting at `reset`.
fn warned(at: i64, weekly: bool, pct: u32, reset: Option<i64>) -> UsageSaid {
    said(at, None, Some(WarningRead { weekly, pct, reset }))
}

/// The reasons of the ledger's rows of `kind`.
fn rows_of(store: &Store, kind: Kind) -> Vec<String> {
    store
        .tail(50)
        .expect("reads")
        .into_iter()
        .filter(|e| e.kind == kind)
        .map(|e| e.why)
        .collect()
}

/// `label`'s record as the store holds it at `now`.
fn held(store: &Store, label: &str, now: i64) -> Account {
    store.load(now).expect("ours").0.account(label)
}

/// A limit row of `window`, written at `at`, that says credits are off.
fn off_row(at: i64, window: &str) -> StandingWall {
    StandingWall {
        at: at.unsigned_abs(),
        until: Some((at + HOUR).unsigned_abs()),
        window: Some(window.to_string()),
        credits: Some(crate::harness::upgrade::CreditsWord::Off {
            reason: Some("org_level_disabled".to_string()),
            until: None,
        }),
    }
}

/// `label`'s credits set to `word` directly.
fn set_credits(store: &Store, label: &str, word: Credits) {
    let mut note = None;
    store
        .transact_within(&LOCK_LADDER, NOW, &mut note, |state| {
            state.accounts.entry(label.to_string()).or_default().credits = Some(word);
            Ok(Vec::new())
        })
        .expect("written");
}

/// Credits off, as a limit row an hour before [`NOW`] said.
fn org_off() -> Credits {
    Credits::Off(CreditsOff {
        reason: "org_level_disabled".to_string(),
        at: NOW - HOUR,
        until: None,
    })
}

/// USAGE CREDITS ARE RECORDED AS AN EPISODE, AND ITS LINE IS ONCE A DAY
/// (accounts v2 §5.4): the first answer seen served on credits begins an
/// episode (a `credits` row, `in use`) that is the day's line; the same
/// answer said again — the loop's retell, another tab, a loop started again
/// — writes nothing, and a newer one inside the episode is its newest; the
/// episode stands to the window's reset, and an answer after it lapsed begins
/// the next. A second episode the same LOCAL day is recorded and raises no
/// line; the next day's does. NEGATIVE CONTROLS: a process the roster does not
/// name, or one on a login of its own, records nothing; a screen that says
/// nothing records nothing.
#[test]
fn credits_are_an_episode_and_its_line_is_once_a_day() {
    let dir = tmp("credits");
    let store = Store::at(dir.path());
    let r = own_row();
    let until = NOW + 2 * HOUR;
    let record = |s: UsageSaid, now: i64| record_usage(&store, &r, "s-1", &stored(), &s, now);
    let noted = |now: i64| credits_noted(&r, &store.load(now).expect("ours").0, now);
    assert_eq!(record(said(NOW, None, None), NOW), Recorded::Unchanged);
    assert!(noted(NOW).is_empty());
    let first = Recorded::Usage {
        account: OWN.to_string(),
        events: 1,
    };
    assert_eq!(record(on_credits(NOW, Some(until)), NOW + 5), first);
    assert_eq!(noted(NOW + 5), [OWN]);
    for now in [NOW + 10 * MIN, NOW + DAY] {
        assert_eq!(
            record(on_credits(NOW, Some(until)), now),
            Recorded::Unchanged,
            "said again at {now}"
        );
    }
    assert_eq!(
        record(on_credits(NOW + HOUR, None), NOW + HOUR),
        Recorded::Usage {
            account: OWN.to_string(),
            events: 0,
        },
        "its newest answer"
    );
    assert_eq!(noted(until - 1), [OWN]);
    assert!(noted(until).is_empty(), "lapsed at the window's reset");
    // A second episode the SAME day: recorded, no line.
    let later = until + HOUR;
    assert_eq!(record(on_credits(later, None), later).as_str(), "usage");
    assert!(noted(later).is_empty(), "once a day");
    assert!(matches!(
        held(&store, OWN, later).credits,
        Some(Credits::InUse(CreditsUse { told: false, .. }))
    ));
    assert_eq!(rows_of(&store, Kind::Credits), ["in use", "in use"]);
    // The next day's: the line again.
    let tomorrow = NOW + DAY;
    record(on_credits(tomorrow, Some(tomorrow + HOUR)), tomorrow);
    assert_eq!(noted(tomorrow), [OWN]);
    // NEGATIVE CONTROLS: no row names the process; a login of its own.
    let other = env_of(&[(LOGIN_ENV, "/logins/elsewhere")]);
    let once = on_credits(NOW, None);
    assert_eq!(
        record_usage(&store, &r, "s-2", &other, &once, NOW),
        Recorded::NotListed
    );
    let own_home = env_of(&[(HOME_ENV, "/elsewhere/.claude")]);
    assert_eq!(
        record_usage(&store, &r, "s-2", &own_home, &once, NOW),
        Recorded::NotStored
    );
}

/// Credits stand [`CREDITS_STALE_S`] past the newest answer served on them,
/// or to the reset of the window they carried past: named, else a session
/// window from their start. NEGATIVE CONTROL: past both, they do not.
#[test]
fn credits_stand_past_their_newest_answer_or_to_their_windows_reset() {
    let c = CreditsUse {
        since: NOW,
        seen: NOW,
        until: None,
        told: true,
    };
    assert!(c.stands(NOW + SESSION_WINDOW_S - 1));
    assert!(!c.stands(NOW + SESSION_WINDOW_S));
    let worked = CreditsUse {
        seen: NOW + 6 * HOUR,
        ..c.clone()
    };
    assert!(worked.stands(NOW + 6 * HOUR + CREDITS_STALE_S - 1));
    assert!(!worked.stands(NOW + 6 * HOUR + CREDITS_STALE_S));
    let named = CreditsUse {
        until: Some(NOW + DAY),
        ..c
    };
    assert!(named.stands(NOW + DAY - 1));
    assert!(!named.stands(NOW + DAY));
}

#[test]
fn an_older_credits_notice_cannot_extend_the_newest_episode() {
    let dir = tmp("credits-order");
    let store = Store::at(dir.path());
    let roster = own_row();
    let record = |sid: &str, at: i64, until: Option<i64>, now: i64| {
        record_usage(&store, &roster, sid, &stored(), &on_credits(at, until), now)
    };
    let update = Recorded::Usage {
        account: OWN.to_string(),
        events: 0,
    };
    assert_eq!(
        record("s-new", NOW, Some(NOW + HOUR), NOW),
        Recorded::Usage {
            account: OWN.to_string(),
            events: 1,
        }
    );
    assert_eq!(record("s-new", NOW + MIN, None, NOW + MIN), update);
    assert!(matches!(
        held(&store, OWN, NOW + MIN).credits,
        Some(Credits::InUse(CreditsUse {
            until: Some(until),
            ..
        })) if until == NOW + HOUR
    ));
    // A redraw of this newest answer may name its reset; a later answer
    // extends the same episode too. Keep both existing update paths.
    assert_eq!(
        record("s-new", NOW + MIN, Some(NOW + 2 * HOUR), NOW + MIN),
        update
    );
    let newest = NOW + 2 * MIN;
    let reset = NOW + 3 * HOUR;
    assert_eq!(record("s-new", newest, Some(reset), newest), update);
    assert_eq!(
        record("s-new", newest, Some(reset), newest + 1),
        Recorded::Unchanged
    );
    let before = held(&store, OWN, reset);
    let Some(Credits::InUse(c)) = &before.credits else {
        panic!("the account retained its episode");
    };
    assert_eq!((c.since, c.seen, c.until), (NOW, newest, Some(reset)));
    assert!(reset > newest + CREDITS_STALE_S);
    assert!(
        !c.stands(reset),
        "the reset and answer freshness both ended"
    );
    let (state, _) = store.load(reset).expect("ours");
    assert!(credits_noted(&roster, &state, reset).is_empty());

    // Another tab still has an older started notice. Its later reset is
    // not a new word, even though its answer falls inside this episode.
    let stale = record("s-old", NOW + MIN / 2, Some(NOW + DAY), reset);
    let (state, _) = store.load(reset).expect("ours");
    let after = state.account(OWN);
    assert!(
        credits_noted(&roster, &state, reset).is_empty(),
        "an older tab's notice revived the digest after the newest episode ended: {:?}",
        after.credits
    );
    let Some(Credits::InUse(c)) = &after.credits else {
        panic!("the account retained its episode");
    };
    assert!(!c.stands(reset));
    assert_eq!(stale, Recorded::Unchanged);
    assert_eq!(after, before);
    assert_eq!(rows_of(&store, Kind::Credits), ["in use"]);
}

/// AN OLDER BUILD'S STATE (no usage keys) reads as none observed, and each
/// word this build writes reads back whole. NEGATIVE CONTROL: a credits word
/// this build does not know reads as none.
#[test]
fn the_usage_keys_round_trip_and_an_older_state_reads_as_none() {
    let mut s = state();
    let older = s.to_json();
    assert!(!older.contains("credits"), "{older}");
    let read = State::from_json(&older, NOW).expect("ours");
    assert_eq!(read.account("main").credits, None);
    let words = [
        Credits::InUse(CreditsUse {
            since: NOW - HOUR,
            seen: NOW - MIN,
            until: Some(NOW + HOUR),
            told: true,
        }),
        Credits::Off(CreditsOff {
            reason: "out_of_credits".to_string(),
            at: NOW - DAY,
            until: Some(NOW + 9 * DAY),
        }),
        Credits::On(NOW - 2 * DAY),
    ];
    for word in words {
        let a = s.accounts.entry("main".to_string()).or_default();
        a.credits = Some(word);
        a.credits_day = Some(NOW.div_euclid(DAY));
        a.warned_session = Some(WindowSaid {
            pct: Some(91),
            reset: Some(NOW + 2 * HOUR),
            at: NOW - MIN,
        });
        a.warned_week = Some(WindowSaid {
            pct: None,
            reset: None,
            at: NOW - HOUR,
        });
        let text = s.to_json();
        let back = State::from_json(&text, NOW).expect("ours");
        assert_eq!(back, s, "{text}");
        assert!(back.account("main").observed());
    }
    // NEGATIVE CONTROL: a word of a later build.
    let text = s.to_json().replace("\"word\":\"on\"", "\"word\":\"maybe\"");
    let back = State::from_json(&text, NOW).expect("ours");
    assert_eq!(back.account("main").credits, None, "{text}");
}

/// WHAT A LIMIT ROW SAYS OF CREDITS IS THE NEWEST WORD ON THEM where it is
/// ([`record_wall`]): `overageStatus: rejected` with its reason and return is
/// [`Credits::Off`]; a newer row that says they are on is [`Credits::On`];
/// one that says nothing changes nothing, and neither does an older one, nor
/// the same row read again.
#[test]
fn a_limit_rows_word_on_credits_is_the_newest_word_on_them() {
    use crate::harness::upgrade::CreditsWord;
    let dir = tmp("credits-off");
    let store = Store::at(dir.path());
    let r = three();
    let off = StandingWall {
        credits: Some(CreditsWord::Off {
            reason: Some("out_of_credits".to_string()),
            until: Some((NOW + 9 * DAY).unsigned_abs()),
        }),
        ..weekly()
    };
    record_wall(&store, &r, "s-one", &stored(), &off, STARTED, NOW);
    let read = |now: i64| held(&store, "main", now).credits;
    let kept = Some(Credits::Off(CreditsOff {
        reason: "out_of_credits".to_string(),
        at: NOW - 10,
        until: Some(NOW + 9 * DAY),
    }));
    assert_eq!(read(NOW), kept);
    let silent = StandingWall {
        at: (NOW + 60).unsigned_abs(),
        ..weekly()
    };
    record_wall(&store, &r, "s-one", &stored(), &silent, STARTED, NOW + 70);
    assert_eq!(read(NOW + 70), kept, "a row that says nothing");
    let older_on = StandingWall {
        at: (NOW - 600).unsigned_abs(),
        credits: Some(CreditsWord::On),
        ..weekly()
    };
    record_wall(
        &store,
        &r,
        "s-one",
        &stored(),
        &older_on,
        NOW - 700,
        NOW + 80,
    );
    assert_eq!(read(NOW + 80), kept, "an older row undoes nothing");
    let on = StandingWall {
        at: (NOW + 120).unsigned_abs(),
        credits: Some(CreditsWord::On),
        ..weekly()
    };
    record_wall(&store, &r, "s-one", &stored(), &on, STARTED, NOW + 130);
    assert_eq!(read(NOW + 130), Some(Credits::On(NOW + 120)));
    assert_eq!(
        record_wall(&store, &r, "s-one", &stored(), &on, STARTED, NOW + 140),
        Recorded::Unchanged,
        "the same row read again"
    );
}

/// THE WEEK NEARLY USED WITH NO CREDITS IS A LINE ([`windows_full`]): the
/// vendor's weekly warning at [`WINDOW_FULL_PCT`] with its reset more than a
/// day off, on an account whose newest word on credits is a limit row's
/// `off`, with that row's reason. NEGATIVE CONTROLS, each beside its rule:
/// credits unknown, on or in use, credits back past their return, a figure
/// under the bar, a reset within a day, and a five-hour warning alone, are no
/// line.
#[test]
fn the_week_nearly_used_with_credits_off_is_a_line_and_nothing_else_is() {
    let r = own_row();
    let line = [(OWN.to_string(), "org_level_disabled".to_string())];
    // `screens` recorded in turn over a fresh store whose credits are
    // `credits`: the lines at the last one's answer.
    let full = |credits: Option<Credits>, screens: &[UsageSaid]| {
        let dir = tmp("window-full");
        let store = Store::at(dir.path());
        if let Some(word) = credits {
            set_credits(&store, OWN, word);
        }
        for s in screens {
            record_usage(&store, &r, "s-1", &stored(), s, s.at);
        }
        let now = screens.last().map_or(NOW, |s| s.at);
        windows_full(&r, &store.load(now).expect("ours").0, now)
    };
    let nearly = warned(NOW, true, 92, Some(NOW + 3 * DAY));
    assert_eq!(full(Some(org_off()), &[nearly]), line);
    assert!(full(None, &[nearly]).is_empty(), "credits unknown");
    assert!(
        full(Some(Credits::On(NOW - HOUR)), &[nearly]).is_empty(),
        "credits on"
    );
    let in_use = Credits::InUse(CreditsUse {
        since: NOW - HOUR,
        seen: NOW - HOUR,
        until: None,
        told: true,
    });
    assert!(full(Some(in_use), &[nearly]).is_empty(), "credits in use");
    let back = Credits::Off(CreditsOff {
        reason: "out_of_credits".to_string(),
        at: NOW - HOUR,
        until: Some(NOW - 1),
    });
    assert!(full(Some(back), &[nearly]).is_empty(), "credits back");
    for (screen, why) in [
        (warned(NOW, true, 89, Some(NOW + 3 * DAY)), "under the bar"),
        (
            warned(NOW, true, 95, Some(NOW + DAY - 1)),
            "a reset within a day",
        ),
        (
            warned(NOW, false, 95, Some(NOW + 3 * HOUR)),
            "the five-hour window",
        ),
    ] {
        assert!(full(Some(org_off()), &[screen]).is_empty(), "{why}");
    }
    // The rows: one per figure that moved, none for the same said again.
    let dir = tmp("window-full-rows");
    let store = Store::at(dir.path());
    for s in [
        nearly,
        nearly,
        warned(NOW + MIN, true, 93, Some(NOW + 3 * DAY)),
        warned(NOW + 2 * MIN, false, 95, Some(NOW + 3 * HOUR)),
    ] {
        record_usage(&store, &r, "s-1", &stored(), &s, NOW + 2 * MIN);
    }
    assert_eq!(
        rows_of(&store, Kind::Warned),
        ["weekly 92%", "weekly 93%", "session 95%"]
    );
}

/// A LIMIT RESET REFILLS THE WINDOW IT WAS AT, AND NO OTHER (the review of
/// S5, rounds 3 to 5): on the one-account machine the vendor warned `96% of
/// your weekly limit` and `91% of your session limit`, credits are off, and
/// the week's line stands ([`windows_full`]). A reset used at the weekly wall
/// — or lifted, the limit had reset on its own — refills the week: the newest
/// word on it ([`WindowSaid`], no figure), so the line ends, and the weekly
/// warning drawn before the reset and said again says nothing; one drawn
/// after it is the newest, and a week that still fills is a line again. A
/// reset at the SESSION wall refills the session window alone: the week is
/// as used as it was, and its line stands — the vendor warns of the week only
/// while little of it has passed (`yS`: none past 60%), so nothing would draw
/// it again before the week's wall. LAST WEEK'S WEEKLY WALL, lapsed and still
/// among the account's walls, walls nothing; the wall the reset was claimed
/// at does, though its outcome is read after it lapsed. NEGATIVE CONTROLS: a
/// reset kept or failed refilled nothing, and the line stands; read as walled,
/// the lapsed wall had the session's reset refill the week; asked at the
/// outcome, the session's lapsed wall kept its warning.
#[test]
fn a_limit_reset_refills_the_walled_window_and_no_other() {
    let r = own_row();
    let week_end = NOW + 3 * DAY;
    let session_end = NOW + 2 * HOUR;
    let line = [(OWN.to_string(), "org_level_disabled".to_string())];
    let refilled = |at: i64| {
        Some(WindowSaid {
            pct: None,
            reset: None,
            at,
        })
    };
    // `end`: when the outcome is read.
    let run = |tag: &str, limit: &str, done: &ResetDone<'_>, end: i64| {
        let dir = tmp(tag);
        let store = Store::at(dir.path());
        let until = if limit == WEEK_LIMIT {
            week_end
        } else {
            session_end
        };
        for s in [
            warned(NOW, true, 96, Some(week_end)),
            warned(NOW + 5, false, 91, Some(session_end)),
        ] {
            record_usage(&store, &r, "s-1", &stored(), &s, s.at);
        }
        set_credits(&store, OWN, org_off());
        let last_week = week_end - 7 * DAY;
        let mut note = None;
        store
            .transact_within(&LOCK_LADDER, NOW + 10, &mut note, |state| {
                let mut rows = note_wall(
                    state,
                    OWN,
                    WEEK_LIMIT,
                    Some(last_week),
                    last_week - HOUR,
                    NOW,
                );
                rows.extend(note_wall(
                    state,
                    OWN,
                    limit,
                    Some(until),
                    NOW + 10,
                    NOW + 10,
                ));
                Ok(rows)
            })
            .expect("walled");
        let (state, _) = store.load(NOW + 20).expect("ours");
        assert_eq!(windows_full(&r, &state, NOW + 20), line, "the control");
        store
            .transact_within(&LOCK_LADDER, NOW + 30, &mut note, |state| {
                let taken = claim_reset(state, OWN, (limit, until), "s-1", 41, START, NOW + 30);
                assert_eq!(taken, ResetClaimed::Taken);
                Ok(reset_outcome(state, OWN, until, "s-1", done, end))
            })
            .expect("recorded");
        let (state, _) = store.load(end + 10).expect("ours");
        let lines = windows_full(&r, &state, end + 10);
        (dir, store, lines, state.account(OWN))
    };
    for done in [
        ResetDone::Used {
            kind: aterm_phase::ResetKind::Weekly,
            left: Some(1),
        },
        ResetDone::Lifted,
    ] {
        let (_dir, store, lines, account) = run("reset-week", WEEK_LIMIT, &done, NOW + 40);
        assert!(lines.is_empty(), "{done:?}: {lines:?}");
        assert_eq!(account.warned_week, refilled(NOW + 40), "{done:?}");
        assert_eq!(
            account.warned_session.and_then(|w| w.pct),
            Some(91),
            "{done:?}"
        );
        let later = NOW + 2 * HOUR;
        let before = warned(NOW, true, 96, Some(week_end));
        assert_eq!(
            record_usage(&store, &r, "s-2", &stored(), &before, later),
            Recorded::Unchanged,
            "{done:?}: drawn before the reset"
        );
        let after = warned(later, true, 97, Some(week_end));
        assert_eq!(
            record_usage(&store, &r, "s-1", &stored(), &after, later).as_str(),
            "usage"
        );
        let (state, _) = store.load(later).expect("ours");
        assert_eq!(windows_full(&r, &state, later), line, "{done:?}");
        // The session's wall, last week's beside it: the week's line stands.
        let (_dir, _store, lines, account) = run("reset-session", SESSION_LIMIT, &done, NOW + 40);
        assert_eq!(lines, line, "{done:?}: the week is as used as it was");
        assert_eq!(account.warned_session, refilled(NOW + 40), "{done:?}");
        assert_eq!(account.warned_week.and_then(|w| w.pct), Some(96));
    }
    // Its outcome read once the session's wall had lapsed: still the wall
    // the reset was claimed at.
    let end = session_end + RESET_GRACE_S + MIN;
    let (_dir, _store, lines, account) =
        run("reset-session-late", SESSION_LIMIT, &ResetDone::Lifted, end);
    assert_eq!(account.warned_session, refilled(end));
    assert_eq!(lines, line, "the week is as used as it was");
    // NEGATIVE CONTROL: nothing refilled.
    for done in [ResetDone::Kept("early"), ResetDone::Failed("unread")] {
        let (_dir, _store, lines, account) = run("reset-week-kept", WEEK_LIMIT, &done, NOW + 40);
        assert_eq!(lines, line, "{done:?}");
        assert_eq!(account.warned_week.and_then(|w| w.pct), Some(96));
    }

    // A sample without a captured warning must stop reporting the exact
    // refilled bucket as low. A model-specific/unknown quota is not either
    // generic bucket, even when the reset timestamp is shared.
    let sampled_claim = |window: &str| {
        let mut s = state();
        note_sample(
            &mut s,
            OWN,
            Sample {
                session_pct: Some(98),
                session_reset: Some(session_end),
                week_pct: Some(99),
                week_reset: Some(week_end),
                at: NOW,
                by: String::new(),
            },
            NOW,
        );
        let until = if window == SESSION_LIMIT {
            session_end
        } else {
            week_end
        };
        note_wall(&mut s, OWN, window, Some(until), NOW + 10, NOW + 10);
        assert_eq!(
            claim_reset(&mut s, OWN, (window, until), "s-1", 41, START, NOW + 30),
            ResetClaimed::Taken
        );
        (s, until)
    };
    for window in [
        WEEK_LIMIT,
        SESSION_LIMIT,
        "seven_day_sonnet",
        "future_quota",
    ] {
        let (mut s, until) = sampled_claim(window);
        reset_outcome(
            &mut s,
            OWN,
            until,
            "s-1",
            &ResetDone::Used {
                kind: aterm_phase::ResetKind::Full,
                left: None,
            },
            NOW + 40,
        );
        let account = s.account(OWN);
        assert_eq!(
            account.week_low_until(NOW + 40),
            (window != WEEK_LIMIT).then_some(week_end),
            "{window}: week"
        );
        assert_eq!(
            account.session_low_until(NOW + 40),
            (window != SESSION_LIMIT).then_some(session_end),
            "{window}: session"
        );
    }

    // A later warning or sample can be fresh consumption after the reset,
    // even when the vendor kept its reset day. A delayed outcome cannot
    // erase either same-period or different-period evidence.
    for (newer_sample, changed_period) in
        [(false, false), (false, true), (true, false), (true, true)]
    {
        let (mut s, until) = sampled_claim(WEEK_LIMIT);
        let new_end = week_end + if changed_period { DAY } else { 0 };
        if newer_sample {
            note_sample(
                &mut s,
                OWN,
                Sample {
                    week_pct: Some(99),
                    week_reset: Some(new_end),
                    at: NOW + 35,
                    ..Sample::default()
                },
                NOW + 35,
            );
        } else {
            note_warning(
                s.accounts.get_mut(OWN).expect("account"),
                OWN,
                WarningRead {
                    weekly: true,
                    pct: 99,
                    reset: Some(new_end),
                },
                NOW + 35,
                NOW + 35,
            );
        }
        reset_outcome(
            &mut s,
            OWN,
            until,
            "s-1",
            &ResetDone::Used {
                kind: aterm_phase::ResetKind::Full,
                left: None,
            },
            NOW + 40,
        );
        assert_eq!(
            s.account(OWN).week_low_until(NOW + 40),
            Some(new_end),
            "sample={newer_sample} changed_period={changed_period}"
        );
    }
}

/// A WARNING DRAWN BEFORE A RESET SAYS NOTHING AFTER IT, ON ANY TAB (the
/// review of S5, round 4): tab B has been idle since its last answer, its
/// footer still drawing `You've used 96% of your weekly limit`, when tab A
/// meets the weekly wall and a reset refills the week. B's loop says the
/// unchanged warning again every [`CREDITS_RETELL_S`], and a loop started
/// again on B reads it as new — each stamped with B's newest answer, older
/// than the refill: nothing is written and no line comes back. NEGATIVE
/// CONTROL: the same warning stamped with the screen read's own time, as the
/// recorder was handed it before, brings the line back minutes after the
/// refill.
#[test]
fn a_warning_drawn_before_a_reset_says_nothing_after_it() {
    let dir = tmp("warned-before-reset");
    let store = Store::at(dir.path());
    let r = own_row();
    let week_end = NOW + 4 * DAY;
    let b = warned(NOW - HOUR, true, 96, Some(week_end));
    record_usage(&store, &r, "s-b", &stored(), &b, NOW - HOUR + 5);
    set_credits(&store, OWN, org_off());
    let mut note = None;
    store
        .transact_within(&LOCK_LADDER, NOW, &mut note, |state| {
            let mut rows = note_wall(state, OWN, WEEK_LIMIT, Some(week_end), NOW, NOW);
            assert_eq!(
                claim_reset(state, OWN, (WEEK_LIMIT, week_end), "s-a", 41, START, NOW),
                ResetClaimed::Taken
            );
            rows.extend(reset_outcome(
                state,
                OWN,
                week_end,
                "s-a",
                &ResetDone::Used {
                    kind: aterm_phase::ResetKind::Weekly,
                    left: None,
                },
                NOW + 10,
            ));
            Ok(rows)
        })
        .expect("reset");
    let full = |now: i64| windows_full(&r, &store.load(now).expect("ours").0, now);
    for now in [NOW + 10 * MIN, NOW + 20 * MIN, NOW + DAY] {
        assert_eq!(
            record_usage(&store, &r, "s-b", &stored(), &b, now),
            Recorded::Unchanged,
            "retold at {now}"
        );
        assert!(full(now).is_empty(), "retold at {now}");
    }
    // NEGATIVE CONTROL: stamped with the read's own time.
    let read_now = UsageSaid {
        at: NOW + 10 * MIN,
        ..b
    };
    assert_eq!(
        record_usage(&store, &r, "s-b", &stored(), &read_now, NOW + 10 * MIN).as_str(),
        "usage"
    );
    assert_eq!(
        full(NOW + 10 * MIN),
        [(OWN.to_string(), "org_level_disabled".to_string())]
    );
}

/// CREDITS SEEN CARRYING A SESSION ARE ON (the review of S5): the owner told
/// that the week is nearly used with credits off ([`windows_full`]) turns
/// them on, and the next crossing goes on, on credits — with credits on, no
/// limit row is written that could say they are. That answer is the newest
/// word on them: no line, then or after the episode ends and the week fills
/// again — and the old row read again (a loop started again over a session
/// still standing at it) is the older word, and says nothing. NEGATIVE
/// CONTROL: a row that said them off INSIDE an episode (its credits ran out)
/// is the newest word: the line stands, through the stale notice said again
/// over no new answer — at the row's own second too, the row being that
/// answer — and the row read again; a newer answer on credits is on again.
#[test]
fn credits_seen_in_use_after_the_row_that_said_them_off_end_its_line() {
    let dir = tmp("credits-back-on");
    let store = Store::at(dir.path());
    let r = own_row();
    let line = [(OWN.to_string(), "org_level_disabled".to_string())];
    let full = |now: i64| windows_full(&r, &store.load(now).expect("ours").0, now);
    let usage = |s: UsageSaid| record_usage(&store, &r, "s-1", &stored(), &s, s.at + 5);
    let wall = |sid: &str, row: &StandingWall, now: i64| {
        record_wall(&store, &r, sid, &stored(), row, STARTED, now);
    };
    let week = |at: i64, pct: u32| warned(at, true, pct, Some(NOW + 3 * DAY));
    wall("s-1", &off_row(NOW - 10, WEEK_LIMIT), NOW);
    usage(week(NOW, 92));
    assert_eq!(full(NOW + 5), line);
    // Turned on: the next crossing goes on, on credits.
    assert_eq!(usage(on_credits(NOW + HOUR, None)).as_str(), "usage");
    assert!(full(NOW + HOUR + 5).is_empty(), "credits are on");
    wall("s-2", &off_row(NOW - 10, WEEK_LIMIT), NOW + HOUR + MIN);
    assert!(full(NOW + HOUR + MIN).is_empty(), "the old row read again");
    // The episode lapses, and the week fills again.
    let ended = NOW + 7 * HOUR;
    usage(week(ended, 95));
    assert!(full(ended + 5).is_empty(), "no word of credits off stands");
    wall("s-2", &off_row(NOW - 10, WEEK_LIMIT), ended + MIN);
    assert!(matches!(
        held(&store, OWN, ended + MIN).credits,
        Some(Credits::InUse(_))
    ));
    assert!(full(ended + MIN).is_empty());
    // NEGATIVE CONTROL: they run out inside the next episode.
    let begun = ended + HOUR;
    usage(on_credits(begun, None));
    let ran_out = off_row(begun + HOUR, WEEK_LIMIT);
    wall("s-1", &ran_out, begun + HOUR + 10);
    assert_eq!(full(begun + HOUR + 10), line);
    for stale in [begun, begun + HOUR] {
        let said_again = on_credits(stale, None);
        assert_eq!(
            record_usage(
                &store,
                &r,
                "s-1",
                &stored(),
                &said_again,
                begun + HOUR + MIN
            ),
            Recorded::Unchanged,
            "the notice at the answer of {stale}"
        );
        assert_eq!(full(begun + HOUR + MIN), line, "its credits ran out");
    }
    wall("s-1", &ran_out, begun + HOUR + 2 * MIN);
    assert_eq!(full(begun + HOUR + 2 * MIN), line, "and read again");
    usage(on_credits(begun + 2 * HOUR, None));
    assert!(full(begun + 2 * HOUR + 5).is_empty(), "on again");
}

/// EACH WINDOW KEEPS ITS OWN WORD (the review of S5): Claude Code shows one
/// warning at a time, the five-hour window's first, so the week nearly used
/// with credits off stays a line ([`windows_full`]) when the footer turns to
/// the session's warning — on a machine whose only reading of the week is
/// the warning — and the week's word keeps the age of the answer it was
/// drawn after. Of one answer, the later read of its screen stands (the
/// footer redrawn since by an answer still too new to stamp it,
/// [`NOTICES_DRAWN_S`]). NEGATIVE CONTROLS: a week's figure ends at its own
/// reset, and one whose reset was not read a week after its answer.
#[test]
fn each_window_keeps_its_own_word() {
    let dir = tmp("warned-windows");
    let store = Store::at(dir.path());
    let r = own_row();
    set_credits(&store, OWN, org_off());
    let usage = |s: UsageSaid| record_usage(&store, &r, "s-1", &stored(), &s, s.at);
    let full = |now: i64| windows_full(&r, &store.load(now).expect("ours").0, now);
    let line = [(OWN.to_string(), "org_level_disabled".to_string())];
    let (week_end, session_end) = (NOW + 2 * DAY, NOW + 3 * HOUR);
    usage(warned(NOW, true, 89, Some(week_end)));
    assert!(full(NOW).is_empty(), "under the bar");
    // The same answer's screen read again, redrawn since: the later read.
    usage(warned(NOW, true, 92, Some(week_end)));
    assert_eq!(full(NOW), line);
    assert_eq!(
        usage(warned(NOW + MIN, false, 91, Some(session_end))).as_str(),
        "usage"
    );
    assert_eq!(full(NOW + MIN), line, "the week is still nearly used");
    let a = held(&store, OWN, NOW + MIN);
    assert_eq!(
        (
            a.warned_week.map(|w| (w.pct, w.at)),
            a.warned_session.and_then(|w| w.pct)
        ),
        (Some((Some(92), NOW)), Some(91))
    );
    // NEGATIVE CONTROLS: the week's own reset; no reset read.
    assert!(full(week_end).is_empty(), "the week has reset");
    usage(warned(NOW + 2 * MIN, true, 94, None));
    let end = NOW + 2 * MIN + WEEK_WINDOW_S;
    assert_eq!(full(end - DAY - 1), line);
    assert!(full(end - DAY).is_empty(), "a week after its answer");
}

/// THE LINE'S WEEK IS THE WARNING'S WORD, NEVER A SAMPLE'S ([`windows_full`];
/// S5 beside S3a's usage tap): on the one-account machine the vendor warned
/// `92% of your weekly limit`, credits are off, and the tap reads the week at
/// 96%. A reset at the weekly wall refills the week: the line ends. The tap's
/// sample keeps its largest figure within the reset ([`note_reading`]), and
/// an idle tab's 96%, read afresh after the reset (a loop started again
/// admits its first reading), is recorded with the reading's own time —
/// ranked beside the warning's word, the week refilled was `nearly used`
/// again. NEGATIVE CONTROLS: a sample, however new and however full, makes no
/// line; a weekly warning drawn after the refill does.
#[test]
fn the_lines_week_is_the_warnings_word_never_a_samples() {
    let dir = tmp("warned-not-sample");
    let store = Store::at(dir.path());
    let r = own_row();
    set_credits(&store, OWN, org_off());
    let week_end = NOW + 4 * DAY;
    let usage = |s: UsageSaid| record_usage(&store, &r, "s-1", &stored(), &s, s.at);
    let full = |now: i64| windows_full(&r, &store.load(now).expect("ours").0, now);
    let line = [(OWN.to_string(), "org_level_disabled".to_string())];
    let edit = |now: i64, f: &dyn Fn(&mut State) -> Vec<Event>| {
        let mut note = None;
        store
            .transact_within(&LOCK_LADDER, now, &mut note, |state| Ok(f(state)))
            .expect("written");
    };
    let tap = |pct: u32| Sample {
        week_pct: Some(pct),
        week_reset: Some(week_end),
        ..Sample::default()
    };
    usage(warned(NOW, true, 92, Some(week_end)));
    edit(NOW + MIN, &|state| {
        note_reading(state, OWN, tap(96), NOW + MIN)
    });
    assert_eq!(full(NOW + MIN), line);
    edit(NOW + 10 * MIN, &|state| {
        let mut rows = note_wall(state, OWN, WEEK_LIMIT, Some(week_end), NOW, NOW + 10 * MIN);
        assert_eq!(
            claim_reset(
                state,
                OWN,
                (WEEK_LIMIT, week_end),
                "s-1",
                41,
                START,
                NOW + 10 * MIN
            ),
            ResetClaimed::Taken
        );
        rows.extend(reset_outcome(
            state,
            OWN,
            week_end,
            "s-1",
            &ResetDone::Used {
                kind: aterm_phase::ResetKind::Weekly,
                left: None,
            },
            NOW + 11 * MIN,
        ));
        rows
    });
    assert!(full(NOW + 12 * MIN).is_empty(), "refilled");
    let later = NOW + 2 * HOUR;
    edit(later, &|state| note_reading(state, OWN, tap(96), later));
    let sample = held(&store, OWN, later).sample.expect("the tap's");
    assert_eq!(
        (sample.week_pct, sample.at),
        (Some(96), later),
        "{sample:?}"
    );
    assert!(
        full(later).is_empty(),
        "the tap's figure, carried past the refill"
    );
    // NEGATIVE CONTROLS: a full sample; a warning after the refill.
    edit(later + MIN, &|state| {
        let read = Sample {
            at: later + MIN,
            ..tap(99)
        };
        note_sample(state, OWN, read, later + MIN)
    });
    assert!(
        full(later + MIN).is_empty(),
        "a sample is no word of the line's"
    );
    usage(warned(later + 2 * MIN, true, 93, Some(week_end)));
    assert_eq!(full(later + 2 * MIN), line);
}

// ---------------------------------------------------------------------------
// The usage tap's readings (design v2 §3.2)
// ---------------------------------------------------------------------------

/// A tap reading of the session window at `pct` resetting at `reset`, and
/// the week at `week`.
fn tapped(pct: u32, reset: i64, week: u32) -> Sample {
    Sample {
        session_pct: Some(pct),
        session_reset: Some(reset),
        week_pct: Some(week),
        week_reset: Some(NOW + 3 * DAY),
        at: 0,
        by: String::new(),
    }
}

/// THE MAX RULE AND THE HEARTBEAT: within one reset the larger figure
/// stands (60 over a later 58, a stale process's); a newer reset takes the
/// newer window whatever its figure; a reading for an OLDER window than the
/// one held is never taken; a held window whose reset passed is void. A
/// reading that changes nothing writes nothing until the heartbeat, which
/// writes the same figures again with a row. NEGATIVE CONTROLS: the probe's
/// `note_sample`, which takes the newer sample whole, does take the 58 — the
/// rule is this function's — and a reading with no window writes nothing.
#[test]
fn a_tap_reading_keeps_the_largest_figure_within_a_window_and_writes_only_news() {
    let mut s = state();
    let reset = NOW + 2 * HOUR;
    let events = note_reading(&mut s, "main", tapped(60, reset, 30), NOW);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, Kind::Sample);
    let held = |s: &State| s.accounts["main"].sample.clone().expect("a sample");
    assert_eq!(held(&s).session_pct, Some(60));
    assert_eq!(held(&s).at, NOW);
    assert_eq!(held(&s).by, "0a0b");
    // 58 in the same window: no change, no write.
    let before = s.clone();
    assert!(note_reading(&mut s, "main", tapped(58, reset, 30), NOW + MIN).is_empty());
    assert_eq!(s, before, "nothing written");
    // The same figures past the heartbeat: written again, the same figures.
    let beat = NOW + READING_HEARTBEAT_S;
    assert_eq!(
        note_reading(&mut s, "main", tapped(58, reset, 30), beat).len(),
        1
    );
    assert_eq!(held(&s).session_pct, Some(60));
    assert_eq!(held(&s).at, beat);
    // A rise in the window is news at once.
    assert_eq!(
        note_reading(&mut s, "main", tapped(61, reset, 30), beat + 1).len(),
        1
    );
    assert_eq!(held(&s).session_pct, Some(61));
    // An older window is never taken; a newer one is, whatever its figure.
    let before = s.clone();
    assert!(note_reading(&mut s, "main", tapped(90, reset - HOUR, 30), beat + 2).is_empty());
    assert_eq!(s, before);
    assert_eq!(
        note_reading(&mut s, "main", tapped(3, reset + 5 * HOUR, 30), beat + 3).len(),
        1
    );
    assert_eq!(held(&s).session_pct, Some(3));
    assert_eq!(held(&s).session_reset, Some(reset + 5 * HOUR));
    // A held window whose reset passed is void: the reading's stands.
    let late = reset + 5 * HOUR + 1;
    assert_eq!(
        note_reading(&mut s, "main", tapped(1, reset - HOUR, 30), late).len(),
        1
    );
    assert_eq!(held(&s).session_pct, Some(1));
    // The week rides the same rule beside it.
    assert_eq!(held(&s).week_pct, Some(30));
    // No window: nothing.
    let before = s.clone();
    let empty = Sample {
        session_pct: None,
        week_pct: None,
        ..tapped(0, reset, 0)
    };
    assert!(note_reading(&mut s, "main", empty, late + DAY).is_empty());
    assert_eq!(s, before);
    // Negative control: the probe's recorder takes the 58 whole.
    let mut probe = state();
    assert_eq!(
        note_sample(
            &mut probe,
            "main",
            Sample {
                at: NOW,
                ..tapped(60, reset, 30)
            },
            NOW
        )
        .len(),
        1
    );
    assert_eq!(
        note_sample(
            &mut probe,
            "main",
            Sample {
                at: NOW + MIN,
                ..tapped(58, reset, 30)
            },
            NOW + MIN
        )
        .len(),
        1
    );
    assert_eq!(
        probe.accounts["main"].sample.as_ref().unwrap().session_pct,
        Some(58)
    );
}

/// ON-SCREEN WARNINGS AND OVERAGE CREDITS ROTATE EARLY BEFORE THE WALL: a
/// five-hour warning at or above [`LOW_SESSION_PCT`] (90%), a weekly warning at
/// or above [`LOW_WEEK_PCT`] (97%), or overage credits carrying a session
/// ([`Credits::InUse`]) marks the subscription [`Condition::Low`], ranks it
/// behind fresh accounts (`WhyNot::LowAheadOfAWall` ahead of a wall, `Rank {
/// low: true, .. }` at one), and moves the session to the next fresh
/// subscription at a turn end in [`Mode::Early`] without typing `/usage`. The
/// 15-minute [`RESET_SOON_S`] guard holds a session whose low window or credits
/// episode resets within 15 minutes while still keeping the account `Low` so no
/// other session rotates onto it. NEGATIVE CONTROLS: warnings below the
/// threshold (89% session, 96% week) and a lapsed credits episode leave the
/// account usable, and a limit reset that refilled the window supersedes an
/// older sample from before the refill.
#[test]
fn warnings_and_credits_in_use_mark_an_account_low_and_rotate_early_ahead_of_the_wall() {
    let r = three();
    let enrolled = all(&r);
    let ahead = Advice::Move {
        to: "work-2".into(),
        ahead: true,
        rotates: true,
        takes_canary: false,
    };
    let ask = |s: &State, now: i64| {
        advise_past_probe(
            &r,
            s,
            &enrolled,
            &me("main"),
            &Point::TurnEnd,
            Mode::Early,
            now,
        )
    };
    let ask_sampled = |s: &State, now: i64| {
        let mut s = s.clone();
        fresh(&mut s, "main");
        advise(
            &r,
            &s,
            &enrolled,
            &me("main"),
            &Point::TurnEnd,
            Mode::Early,
            now,
        )
    };
    let with_warned = |weekly: bool, pct: u32, reset: Option<i64>| {
        let mut s = on_main();
        fresh(&mut s, "work-2");
        let w = WindowSaid {
            pct: Some(pct),
            reset,
            at: NOW,
        };
        let acct = s.accounts.entry("main".into()).or_default();
        if weekly {
            acct.warned_week = Some(w);
        } else {
            acct.warned_session = Some(w);
        }
        s
    };

    // 1. Five-hour session warning >= 90% rotates early; 89% does not.
    let s_warn = with_warned(false, 90, Some(NOW + 2 * HOUR));
    assert_eq!(cond(&r, &s_warn, "main", NOW), Condition::Low);
    assert_eq!(s_warn.accounts["main"].low_until(NOW), Some(NOW + 2 * HOUR));
    assert_eq!(ask(&s_warn, NOW), ahead);
    assert_eq!(ask_sampled(&s_warn, NOW), ahead);
    assert!(
        candidates(&r, &s_warn, &enrolled, Some("work-2"), &Point::TurnEnd, NOW)
            .contains(&("main".to_string(), Err(WhyNot::LowAheadOfAWall)))
    );
    let s_sub = with_warned(false, 89, Some(NOW + 2 * HOUR));
    assert_eq!(cond(&r, &s_sub, "main", NOW), Condition::Usable);

    // 2. Weekly warning >= 97% rotates early and supplies week_reset; 96% does not.
    let w_end = NOW + 3 * DAY;
    let w_warn = with_warned(true, 97, Some(w_end));
    assert_eq!(cond(&r, &w_warn, "main", NOW), Condition::Low);
    assert_eq!(w_warn.accounts["main"].low_until(NOW), Some(w_end));
    assert_eq!(w_warn.accounts["main"].week_reset(NOW), Some(w_end));
    assert_eq!(ask(&w_warn, NOW), ahead);
    assert_eq!(ask_sampled(&w_warn, NOW), ahead);
    let w_sub = with_warned(true, 96, Some(w_end));
    assert_eq!(cond(&r, &w_sub, "main", NOW), Condition::Usable);
    assert_eq!(w_sub.accounts["main"].week_reset(NOW), Some(w_end));

    // 3. Overage credits carrying a session (`Credits::InUse`) mark the account
    // Low and rotate early to a subscription with included quota; once the
    // episode ends, the account is Usable again.
    let with_credits = |until: Option<i64>| {
        let mut s = on_main();
        fresh(&mut s, "work-2");
        s.accounts.entry("main".into()).or_default().credits = Some(Credits::InUse(CreditsUse {
            since: NOW,
            seen: NOW,
            until,
            told: true,
        }));
        s
    };
    let c_end = NOW + 2 * HOUR;
    let c_in_use = with_credits(Some(c_end));
    assert_eq!(cond(&r, &c_in_use, "main", NOW), Condition::Low);
    assert_eq!(c_in_use.accounts["main"].low_until(NOW), Some(c_end));
    assert_eq!(ask(&c_in_use, NOW), ahead);
    assert_eq!(ask_sampled(&c_in_use, NOW), ahead);
    assert_eq!(
        cond(&r, &c_in_use, "main", c_end),
        Condition::Usable,
        "NEGATIVE CONTROL: lapsed credits episode"
    );

    // 4. The 15-minute RESET_SOON_S guard holds a session whose warning resets
    // within 15 minutes while still keeping the account Low for candidate
    // selection; overage credits already in use still rotate immediately.
    for mut near in [
        with_warned(false, 95, Some(NOW + RESET_SOON_S)),
        with_warned(true, 98, Some(NOW + RESET_SOON_S)),
    ] {
        fresh(&mut near, "main");
        assert_eq!(cond(&r, &near, "main", NOW), Condition::Low);
        assert!(!near.accounts["main"].leave_early(NOW));
        assert_eq!(ask(&near, NOW), Advice::Stay(StayWhy::NothingDue));
    }
    let near_credits = with_credits(Some(NOW + RESET_SOON_S));
    assert!(near_credits.accounts["main"].leave_early(NOW));
    assert_eq!(ask(&near_credits, NOW), ahead);

    // 5. NEGATIVE CONTROL: a limit reset that refilled the window (`pct: None`
    // at `w.at >= s.at`) supersedes a low sample from before the refill.
    let mut refilled = on_main();
    refilled.accounts.insert(
        "main".into(),
        Account {
            sample: Some(Sample {
                at: NOW - MIN,
                ..sample(95, 99)
            }),
            warned_session: Some(WindowSaid {
                pct: None,
                reset: Some(NOW + 3 * HOUR),
                at: NOW,
            }),
            warned_week: Some(WindowSaid {
                pct: None,
                reset: Some(NOW + 3 * DAY),
                at: NOW,
            }),
            ..Account::default()
        },
    );
    assert_eq!(cond(&r, &refilled, "main", NOW), Condition::Usable);
    assert!(!refilled.accounts["main"].leave_early(NOW));
}
