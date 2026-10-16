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

const REFERENCE: &str = r#"
# The owner's accounts.
[[account]]
label = "main"             # no dir: the login this machine already has
class = "dedicated"

[[account]]
label = "work-2"
class = "dedicated"
dir = "/logins/work-2"

[[account]]
label = "pool-a"
class = "shared"
dir = "/logins/pool-a"
"#;

#[test]
fn a_roster_admits_whole_in_file_order_and_a_missing_file_is_the_empty_roster() {
    let parsed = Roster::parse(REFERENCE).expect("the reference roster admits");
    assert_eq!(parsed, three());
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
    std::fs::write(&path, REFERENCE).expect("write");
    assert_eq!(Roster::load(&path), Ok(three()));

    // NEGATIVE CASES for the file itself: unreadable, not text, too large.
    let err = Roster::load(dir.path()).expect_err("a directory is not a roster");
    assert!(err.contains(&dir.path().display().to_string()), "{err}");
    std::fs::write(&path, [0xff, 0xfe, b'\n']).expect("write");
    assert!(Roster::load(&path).expect_err("bytes").contains("UTF-8"));
    let big = "# pad\n".repeat(usize::try_from(MAX_ROSTER_BYTES / 6 + 1).expect("fits"));
    std::fs::write(&path, big).expect("write");
    assert!(
        Roster::load(&path)
            .expect_err("too large")
            .contains("larger than")
    );
}

/// `roster_text` is the file's text, NOT its admission: what `accounts add`
/// appends to. A missing file is no text; a file that refuses still reads
/// back whole (negative control: `Roster::load` refuses the same file); a
/// file that is not text is an error naming its path.
#[test]
fn the_roster_text_is_read_bounded_and_is_not_an_admission() {
    let dir = tmp("roster-text");
    let path = dir.path().join(ACCOUNTS_FILE);
    assert_eq!(roster_text(&path), Ok(None), "a missing file is no text");
    std::fs::write(&path, REFERENCE).expect("write");
    assert_eq!(roster_text(&path), Ok(Some(REFERENCE.to_string())));

    let refused = "[[account]]\nlabel = \"main\"\nclass = \"both\"\n";
    std::fs::write(&path, refused).expect("write");
    assert_eq!(roster_text(&path), Ok(Some(refused.to_string())));
    assert!(Roster::load(&path).is_err(), "the same file does not admit");

    std::fs::write(&path, [0xff, 0xfe, b'\n']).expect("write");
    let err = roster_text(&path).expect_err("bytes");
    assert!(err.contains("UTF-8"), "{err}");
    assert!(err.contains(&path.display().to_string()), "{err}");
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
        (
            second("label = \"Main\"\nclass = \"shared\"\n"),
            "row 2: label `Main` is outside",
        ),
        (
            second("label = \"a b\"\nclass = \"shared\"\n"),
            "label `a b` is outside",
        ),
        (
            second("label = \"-x\"\nclass = \"shared\"\n"),
            "label `-x` is outside",
        ),
        (
            second("label = \".x\"\nclass = \"shared\"\n"),
            "label `.x` is outside",
        ),
        (
            second("label = \"\"\nclass = \"shared\"\n"),
            "row 2: label `` is outside",
        ),
        (
            second(&format!("label = \"{long_label}\"\nclass = \"shared\"\n")),
            "is outside",
        ),
        // In the grammar and still no label: the harness command's help
        // word, refused by name.
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
            second("label = \"w\"\nclass = \"shared\"\ndir = \"\"\n"),
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
    for (text, needle) in &cases {
        let err = Roster::parse(text).expect_err(&format!("{text:?} must be refused"));
        assert!(err.starts_with(ACCOUNTS_FILE), "{err}");
        assert!(
            err.contains(needle),
            "the refusal must name {needle:?}: {err}"
        );
    }

    // The row bound is a refusal, not a truncation — and its positive twin.
    let many = |n: usize| -> String {
        (0..n)
            .map(|i| {
                format!("[[account]]\nlabel = \"a{i}\"\nclass = \"shared\"\ndir = \"/x{i}\"\n")
            })
            .collect()
    };
    assert_eq!(
        Roster::parse(&many(MAX_ACCOUNTS)).map(|r| r.rows.len()),
        Ok(MAX_ACCOUNTS)
    );
    let err = Roster::parse(&many(MAX_ACCOUNTS + 1)).expect_err("over the bound");
    assert!(
        err.contains("33 rows") && err.contains("at most 32"),
        "{err}"
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
        assert!(
            label_refusal(bad).is_some_and(|why| why.starts_with("is outside")),
            "{bad}"
        );
    }
    // `help` is the one word of the grammar that is no label: the harness
    // command reads it as its help word wherever it stands, so an account
    // named so could be listed and never named. NEGATIVE CONTROL: a label
    // that only begins with it is one.
    assert_eq!(RESERVED_LABEL, "help");
    assert!(!admissible_label("help"));
    assert!(label_refusal("help").is_some_and(|why| why.contains("help word")));
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
    ];
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

    // The default login rides as the variable SET AND EMPTY.
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
}

#[test]
fn a_process_is_matched_to_its_row_by_its_own_login_variable() {
    let r = three();
    assert_eq!(
        r.row_for_login(None).map(|r| r.label.as_str()),
        Some("main")
    );
    assert_eq!(
        r.row_for_login(Some("")).map(|r| r.label.as_str()),
        Some("main")
    );
    assert_eq!(
        r.row_for_login(Some("/logins/work-2"))
            .map(|r| r.label.as_str()),
        Some("work-2")
    );
    // NEGATIVE CONTROLS: a near miss is no row — never the nearest one.
    for miss in [
        "/logins/work-2/",
        "/logins/Work-2",
        "/logins/work-2 ",
        "logins/work-2",
        "/logins",
    ] {
        assert_eq!(r.row_for_login(Some(miss)), None, "{miss:?}");
    }
    // With no row for this machine's own login, the default login is no row.
    let no_default = roster(&[("work-2", Class::Dedicated, Some("/logins/work-2"))]);
    assert_eq!(no_default.row_for_login(None), None);
    assert_eq!(no_default.row_for_login(Some("")), None);
}

#[test]
fn append_row_writes_only_what_the_next_read_admits() {
    let one = append_row("", "main", Class::Dedicated, None).expect("the first row");
    assert_eq!(
        one,
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n"
    );
    // An existing file with no final line break still gets a clean block.
    let two = append_row(
        one.trim_end(),
        "pool-a",
        Class::Shared,
        Some("/logins/pool-a"),
    )
    .expect("the second row");
    assert_eq!(
        Roster::parse(&two).expect("admits").rows,
        roster(&[
            ("main", Class::Dedicated, None),
            ("pool-a", Class::Shared, Some("/logins/pool-a")),
        ])
        .rows
    );
    assert!(two.starts_with(one.trim_end()), "what was there is kept");

    // NEGATIVE CASES, each by name, and nothing is handed back to write.
    let cases: [(&str, &str, Option<&str>, &str); 7] = [
        (
            &two,
            "main",
            Some("/logins/other"),
            "label `main` is listed twice",
        ),
        (
            &two,
            "help",
            Some("/logins/h"),
            "label `help` is the harness",
        ),
        (&two, "work-2", Some("/logins/pool-a"), "name one dir"),
        (&two, "work-2", None, "both name no dir"),
        (&two, "Work", Some("/logins/w"), "is outside"),
        (&two, "work-2", Some("logins/w"), "its dir is relative"),
        (
            "oops = 1\n",
            "work-2",
            Some("/logins/w"),
            "unknown key `oops`",
        ),
    ];
    for (existing, label, dir, needle) in cases {
        let err = append_row(existing, label, Class::Shared, dir).expect_err(needle);
        assert!(err.contains(needle), "{needle:?}: {err}");
    }
    assert_eq!(row_toml("Work", Class::Shared, None), None);
    assert_eq!(row_toml("help", Class::Shared, None), None);
    assert_eq!(row_toml("work", Class::Shared, Some("relative")), None);

    // A ROSTER THAT DEFINES `account` INLINE admits, and an [[account]]
    // table cannot extend it. The refusal says THAT — not that the owner's
    // file is bad TOML, which it is not — and carries the row to add by
    // hand, in the spelling that array takes.
    let inline = "account = [ { label = \"main\", class = \"dedicated\" } ]\n";
    assert_eq!(Roster::parse(inline).map(|r| r.rows.len()), Ok(1));
    let odd = "/logins/it's \"here\"/";
    let err = append_row(inline, "work-2", Class::Shared, Some(odd)).expect_err("inline");
    assert!(err.contains("defines `account` inline"), "{err}");
    assert!(!err.contains("not valid TOML"), "{err}");
    let by_hand = row_inline("work-2", Class::Shared, Some(odd)).expect("a row");
    assert!(err.ends_with(&by_hand), "{err}");
    // That row, put where the refusal says, is read back as the row asked
    // for — the dir byte for byte.
    let fixed = inline.replace(" ]", &format!(", {by_hand} ]"));
    assert_eq!(
        Roster::parse(&fixed).expect("admits").rows[1],
        Row {
            label: "work-2".into(),
            class: Class::Shared,
            dir: Some(odd.into()),
            ..Row::default()
        }
    );
    assert_eq!(
        row_inline("main", Class::Dedicated, None).as_deref(),
        Some("{ label = \"main\", class = \"dedicated\" }")
    );
    assert_eq!(row_inline("help", Class::Shared, None), None);
    assert_eq!(row_inline("w", Class::Shared, Some("relative")), None);
    // NEGATIVE CONTROL: the same rows as [[account]] tables are appended to.
    assert!(append_row(&one, "work-2", Class::Shared, Some(odd)).is_ok());

    // The 33rd row is refused before it is written.
    let mut full = String::new();
    for i in 0..MAX_ACCOUNTS {
        full = append_row(
            &full,
            &format!("a{i}"),
            Class::Shared,
            Some(&format!("/x{i}")),
        )
        .expect("fits");
    }
    let err = append_row(&full, "one-more", Class::Shared, Some("/more")).expect_err("the bound");
    assert!(err.contains("at most 32"), "{err}");
}

/// ENROLLED MEANS A LOGIN WAS MADE: a row with a `dir` counts only where
/// that directory holds at least one entry ([`login_made`]) — what the
/// vendor's flow writes when it is run with the directory as its home. The
/// directory `accounts add` makes, with nothing in it yet, is LISTED AND NOT
/// ENROLLED: `not-enrolled` in the table, and no candidate for a session at
/// a wall. One entry makes it enrolled: `unseen`, and the candidate.
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
    // The empty directory is no candidate at a wall; the login made is.
    let ranked = |here: &BTreeSet<String>| {
        candidates(&r, &s, here, Some("main"), &wall(), NOW)
            .into_iter()
            .map(|(label, verdict)| (label, verdict.map(|_| ()).map_err(WhyNot::as_str)))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ranked(&here),
        [
            ("work-2".to_string(), Ok(())),
            ("main".to_string(), Err("this session's own")),
            ("check-2".to_string(), Err("not-enrolled")),
            ("pool-a".to_string(), Err("not-enrolled")),
            ("pool-b".to_string(), Err("not-enrolled")),
        ]
    );

    // ONE ENTRY makes it enrolled: unseen, and a candidate.
    std::fs::write(empty.join("a-login-file"), "{}").expect("the login is made");
    let here = enrolled(&r);
    assert_eq!(labels(&r), ["check-2", "main", "work-2"]);
    assert_eq!(cond("check-2", &here), Condition::Unseen);
    assert!(
        ranked(&here).contains(&("check-2".to_string(), Ok(()))),
        "{:?}",
        ranked(&here)
    );

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
    assert_eq!(
        State::from_json(&text, NOW).expect("reads").to_json(),
        text,
        "stable"
    );

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
    assert_eq!(
        doc["accounts"]["work-2"]["before"]["session_pct"].as_u64(),
        Some(10)
    );

    assert!(doc["canary"].is_null(), "a proven account has no canary");
    assert_eq!(
        doc["accounts"]["main"]["cleared_at"].as_i64(),
        Some(NOW - 20)
    );
    assert!(doc["accounts"]["work-2"]["cleared_at"].is_null());

    // When a session last ran on an account, and `null` for one nobody ran
    // on; the reset claim with its process and its outcome.
    assert_eq!(doc["accounts"]["main"]["ran_at"].as_i64(), Some(NOW - 3));
    assert_eq!(doc["accounts"]["work-2"]["ran_at"].as_i64(), Some(NOW - 2));
    assert!(doc["accounts"]["pool-a"]["ran_at"].is_null());
    assert_eq!(
        aterm_json::to_string(&doc["reset"]).expect("text"),
        format!(
            "{{\"account\":\"main\",\"at\":{},\"done\":\"kept\",\"pid\":4242,\
             \"sid\":\"s-1a2b\",\"start\":\"1790000000.250000\",\"until\":{}}}",
            NOW - 4,
            NOW + HOUR
        )
    );
    // A claim in flight has no outcome: `null`, and it reads back as none.
    let mut flying = s.clone();
    flying.reset.as_mut().expect("the claim").done = None;
    let text_flying = flying.to_json();
    assert!(text_flying.contains("\"done\":null"), "{text_flying}");
    assert_eq!(State::from_json(&text_flying, NOW), Ok(flying));

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
    for key in ["pid", "start", "done"] {
        assert!(at(&mut old, &["reset"]).remove(key).is_some(), "{key}");
    }
    let read = State::from_json(&aterm_json::to_string(&old).expect("text"), NOW).expect("loads");
    let mut want = s.clone();
    for account in want.accounts.values_mut() {
        account.ran_at = None;
    }
    want.reset = Some(ResetClaim {
        pid: 0,
        start: String::new(),
        done: None,
        ..s.reset.clone().expect("the claim")
    });
    assert_eq!(read, want);
    // NEGATIVE CONTROL: the keys were what differed.
    assert_ne!(read, s);
    // A key of the wrong shape is the unknown it stands for, and a stamp
    // ahead of the clock is read as made now, as this machine's others are.
    let mut odd: Value = aterm_json::from_str(&text).expect("JSON");
    at(&mut odd, &["accounts", "main"]).insert("ran_at".into(), Value::from(NOW + DAY));
    at(&mut odd, &["accounts", "work-2"]).insert("ran_at".into(), Value::from("yesterday"));
    at(&mut odd, &["reset"]).insert("pid".into(), Value::from("4242"));
    at(&mut odd, &["reset"]).insert("start".into(), Value::from(7_i64));
    at(&mut odd, &["reset"]).insert("done".into(), Value::from(true));
    let read = State::from_json(&aterm_json::to_string(&odd).expect("text"), NOW).expect("loads");
    assert_eq!(read.accounts["main"].ran_at, Some(NOW));
    assert_eq!(read.accounts["work-2"].ran_at, None);
    assert_eq!(
        read.reset
            .as_ref()
            .map(|c| (c.pid, c.start.as_str(), c.done.as_deref())),
        Some((0, "", None))
    );

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

    // NEGATIVE CASES: each is an `Err`, never a guess and never a panic —
    // and each says which kind of file it was. Text that is not JSON is a
    // file cut short, which holds nothing; JSON that is not this build's
    // state is somebody's state, and is not ours to replace.
    let good = s.to_json();
    let torn = &good[..good.len() / 2];
    let cases: [(String, &str, bool); 8] = [
        (
            good.replacen("\"schema\":1", "\"schema\":2", 1),
            "schema 2",
            false,
        ),
        (
            good.replacen("\"schema\":1", "\"schema\":\"1\"", 1),
            "names no schema",
            false,
        ),
        (
            good.replacen("\"schema\":1,", "", 1),
            "names no schema",
            false,
        ),
        (torn.to_string(), "not JSON", true),
        (String::new(), "not JSON", true),
        ("[1, 2]".to_string(), "not a JSON object", false),
        ("null".to_string(), "not a JSON object", false),
        (format!("{good}{good}"), "not JSON", true),
    ];
    for (text, needle, is_torn) in &cases {
        let err = State::from_json(text, NOW).expect_err(needle);
        assert!(err.why().contains(needle), "{needle:?}: {err:?}");
        assert_eq!(matches!(err, Unread::Torn(_)), *is_torn, "{needle:?}");
        assert_eq!(matches!(err, Unread::NotOurs(_)), !*is_torn, "{needle:?}");
    }
    // Ten thousand open brackets are an error, not a stack overflow.
    assert!(matches!(
        State::from_json(&"[".repeat(10_000), NOW),
        Err(Unread::Torn(_))
    ));
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
    assert_eq!(STAMP_SKEW_S, 300);
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
        left: 1,
        use_by: Some(ahead + DAY),
        at: ahead,
        by: "0a0b".into(),
    });
    main.auth = Auth::SignedOut;
    main.auth_at = Some(ahead);
    main.cleared_at = Some(ahead);
    let text = s.to_json();

    let read = State::from_json(&text, NOW).expect("reads");
    let main = &read.accounts["main"];
    let cap = NOW + STAMP_SKEW_S;
    assert_eq!(main.walls["five_hour"].at, cap);
    assert_eq!(main.sample.as_ref().map(|x| x.at), Some(cap));
    assert_eq!(main.offer.as_ref().map(|x| x.at), Some(cap));
    // This machine's own stamps have no other clock to allow for.
    assert_eq!((main.auth_at, main.cleared_at), (Some(NOW), Some(NOW)));
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
    assert!(!note_offer(&mut s, "", 1, None, NOW, NOW));
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

/// [`signed_out`]: the enrolled rows whose last word is signed out, in ROSTER
/// order whatever order the state keeps them in. NEGATIVE CONTROLS: a
/// signed-out row nobody enrolled is left out, a limited row is left out, and
/// an empty state names none.
#[test]
fn the_signed_out_labels_are_the_enrolled_ones_in_roster_order() {
    let r = three();
    let mut s = state();
    // Noted in the state's own (sorted) order, which is not the roster's.
    note_auth(&mut s, "pool-a", Auth::SignedOut, NOW);
    note_auth(&mut s, "work-2", Auth::SignedOut, NOW);
    assert!(
        s.accounts
            .keys()
            .map(String::as_str)
            .eq(["pool-a", "work-2"])
    );
    assert_eq!(signed_out(&r, &s, &all(&r), NOW), ["work-2", "pool-a"]);
    note_auth(&mut s, "main", Auth::SignedOut, NOW);
    assert_eq!(
        signed_out(&r, &s, &all(&r), NOW),
        ["main", "work-2", "pool-a"]
    );
    // A sign-in ends it for that account alone.
    note_auth(&mut s, "work-2", Auth::Ok, NOW + 1);
    assert_eq!(signed_out(&r, &s, &all(&r), NOW + 1), ["main", "pool-a"]);

    // NEGATIVE CONTROL: signed out and not enrolled is `not-enrolled`; the row
    // with no `dir` is enrolled whatever the set holds.
    let here: BTreeSet<String> = ["work-2".to_string()].into();
    note_auth(&mut s, "work-2", Auth::SignedOut, NOW + 2);
    assert_eq!(
        condition(r.row("pool-a").expect("a row"), &s, false, NOW),
        Condition::NotEnrolled
    );
    assert_eq!(signed_out(&r, &s, &here, NOW + 2), ["main", "work-2"]);
    assert_eq!(signed_out(&r, &s, &BTreeSet::new(), NOW + 2), ["main"]);

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

    // NEGATIVE CONTROL: nothing observed, nothing named.
    assert_eq!(
        signed_out(&r, &state(), &all(&r), NOW),
        Vec::<String>::new()
    );
    assert_eq!(
        signed_out(&Roster::default(), &s, &BTreeSet::new(), NOW),
        Vec::<String>::new()
    );
}

#[test]
fn a_wall_stands_until_its_instant_and_not_a_second_longer() {
    let r = three();
    let wall = |until: Option<i64>, at: i64, attempts: u32| Wall {
        until,
        at,
        by: String::new(),
        attempts,
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
        assert_eq!(w.stands_until(), end, "{w:?}");
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
    // The named constants are the design's numbers.
    assert_eq!(RESET_GRACE_S, 150);
    assert_eq!(LATE_WALL_S, [600, 1800]);
    assert_eq!(NO_RESET_WALL_S, [1800, 7200, 21600]);

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
    assert_eq!(wall(Some(i64::MAX), 0, 0).stands_until(), i64::MAX);
    assert_eq!(wall(None, i64::MAX, 3).stands_until(), i64::MAX);
}

#[test]
fn low_is_a_running_window_at_its_threshold_however_near_its_reset() {
    let r = three();
    let at = |s: Sample, now: i64| {
        let mut st = state();
        st.accounts.entry("work-2".into()).or_default().sample = Some(s);
        cond(&r, &st, "work-2", now)
    };
    assert_eq!((LOW_SESSION_PCT, LOW_WEEK_PCT, RESET_SOON_S), (90, 97, 900));

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
    // An account with NOTHING left, ten minutes from its reset: low, never
    // fresh.
    let spent = Sample {
        session_reset: Some(NOW + 10 * MIN),
        ..sample(100, 20)
    };
    assert_eq!(at(spent.clone(), NOW), Condition::Low);
    assert!(!at(spent.clone(), NOW).fresh());
    assert_eq!(at(spent, NOW + 10 * MIN), Condition::Usable, "its reset");

    // The guard, where it belongs: a session is worth moving off only an
    // account whose low window is MORE than 15 minutes from its reset.
    assert!(session(NOW + 901).leave_early(NOW));
    assert!(!session(NOW + 900).leave_early(NOW));
    assert!(!session(NOW + 1).leave_early(NOW));
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
    assert_eq!((SESSION_WINDOW_S, WEEK_WINDOW_S), (18_000, 604_800));

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
    assert_eq!(first[0].why, format!("five_hour until {reset}"));
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
        assert_eq!(events[0].why, "overage, no reset named");
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
    let mut s = on_main();
    limited(&mut s, "main", NOW + HOUR);
    let enrolled = all(&r);
    let go = |roster: &Roster, me: &Me<'_>, mode: Mode| {
        advise(roster, &s, &enrolled, me, &wall(), mode, NOW)
    };
    // The positive twin: this session, at this wall, moves.
    assert_eq!(
        go(&r, &me("main"), Mode::Early),
        Advice::Move {
            to: "work-2".into(),
            ahead: false,
            rotates: true,
            takes_canary: false
        }
    );
    assert_eq!(go(&r, &me("main"), Mode::Off), Advice::Stay(StayWhy::Off));
    assert_eq!(
        go(&Roster::default(), &me("main"), Mode::Early),
        Advice::Stay(StayWhy::NoRoster)
    );
    let unlisted = Me {
        account: None,
        ..me("main")
    };
    assert_eq!(
        go(&r, &unlisted, Mode::Early),
        Advice::Stay(StayWhy::NotListed)
    );
    assert_eq!(
        go(&r, &me("gone"), Mode::Early),
        Advice::Stay(StayWhy::NotListed)
    );
    let other_home = Me {
        default_home: false,
        ..me("main")
    };
    assert_eq!(
        go(&r, &other_home, Mode::Early),
        Advice::Stay(StayWhy::OtherHome)
    );
    let api_key = Me {
        stored_login: false,
        ..me("main")
    };
    assert_eq!(
        go(&r, &api_key, Mode::Wall),
        Advice::Stay(StayWhy::NotAStoredLogin)
    );
}

#[test]
fn every_word_reads_back_and_nothing_else_is_a_word() {
    for class in [Class::Dedicated, Class::Shared] {
        assert_eq!(Class::parse(class.as_str()), Some(class));
    }
    assert!(Class::Dedicated < Class::Shared, "dedicated sorts first");
    for mode in [Mode::Early, Mode::Wall, Mode::Off] {
        assert_eq!(Mode::parse(mode.as_str()), Some(mode));
    }
    for not_a_word in ["", "Dedicated", "team", "EARLY", "true", "boundary"] {
        assert_eq!(Class::parse(not_a_word), None, "{not_a_word}");
        assert_eq!(Mode::parse(not_a_word), None, "{not_a_word}");
    }
    let whys = [
        (StayWhy::Off, "off"),
        (StayWhy::NoRoster, "no-roster"),
        (StayWhy::NotListed, "not-listed"),
        (StayWhy::OtherHome, "other-home"),
        (StayWhy::NotAStoredLogin, "not-a-stored-login"),
        (StayWhy::Canary, "canary"),
        (StayWhy::NoCandidate, "no-candidate"),
        (StayWhy::Breaker { until: 0 }, "breaker"),
        (StayWhy::Pinned, "pinned"),
        (StayWhy::OfferExpiring, "offer-expiring"),
        (StayWhy::NoFresh, "no-fresh"),
        (StayWhy::NothingDue, "nothing-due"),
    ];
    for (why, word) in whys {
        assert_eq!(why.as_str(), word);
    }
    // The advice's own word, for a host's log: one each.
    let advices = [
        (Advice::Stay(StayWhy::NothingDue), "stay"),
        (Advice::Probe, "probe"),
        (
            Advice::Move {
                to: "work-2".into(),
                ahead: false,
                rotates: true,
                takes_canary: false,
            },
            "move",
        ),
        (Advice::Wait { until: NOW }, "wait"),
        (Advice::Resume { cleared_at: NOW }, "resume"),
    ];
    for (advice, word) in advices {
        assert_eq!(advice.as_str(), word);
    }
}

#[test]
fn rule_2_follows_an_active_account_that_is_proven_and_fresh_or_pinned() {
    let r = three();
    let enrolled = all(&r);
    let mut s = on_main();
    s.active = Some("work-2".into());
    let ask = |s: &State, point: &Point, mode: Mode| {
        advise(&r, s, &enrolled, &me("main"), point, mode, NOW)
    };
    let follow = |ahead: bool| Advice::Move {
        to: "work-2".into(),
        ahead,
        rotates: false,
        takes_canary: false,
    };

    // Proven and fresh: at ANY point, in either mode — a healthy `main`
    // included.
    assert_eq!(ask(&s, &Point::TurnEnd, Mode::Early), follow(true));
    assert_eq!(ask(&s, &Point::TurnEnd, Mode::Wall), follow(true));
    assert_eq!(ask(&s, &wall(), Mode::Wall), follow(false));

    // NEGATIVE CONTROL: not proven, with its canary's hold standing, nobody
    // follows — and nobody rotates past it either, at a wall or away from
    // one.
    s.proven = false;
    s.canary = Some(Canary {
        sid: "s-two".into(),
        at: NOW - MIN,
        took: false,
    });
    assert_eq!(
        ask(&s, &Point::TurnEnd, Mode::Early),
        Advice::Stay(StayWhy::Canary)
    );
    assert_eq!(ask(&s, &wall(), Mode::Early), Advice::Stay(StayWhy::Canary));
    s.proven = true;
    s.canary = None;

    // An active account that cannot be run on is not followed: the rules
    // below decide.
    let mut walled = s.clone();
    limited(&mut walled, "work-2", NOW + HOUR);
    assert_eq!(
        ask(&walled, &Point::TurnEnd, Mode::Wall),
        Advice::Stay(StayWhy::NothingDue)
    );
    assert_eq!(
        ask(&walled, &wall(), Mode::Wall),
        Advice::Move {
            to: "pool-a".into(),
            ahead: false,
            rotates: true,
            takes_canary: false
        }
    );
    let mut out = s.clone();
    note_auth(&mut out, "work-2", Auth::SignedOut, NOW);
    assert_eq!(
        ask(&out, &Point::TurnEnd, Mode::Wall),
        Advice::Stay(StayWhy::NothingDue)
    );
    let mut gone = enrolled.clone();
    gone.remove("work-2");
    assert_eq!(
        advise(&r, &s, &gone, &me("main"), &Point::TurnEnd, Mode::Wall, NOW),
        Advice::Stay(StayWhy::NothingDue)
    );
    // The session already on the active account has nobody to follow.
    assert_eq!(
        advise(
            &r,
            &s,
            &enrolled,
            &me("work-2"),
            &Point::TurnEnd,
            Mode::Wall,
            NOW
        ),
        Advice::Stay(StayWhy::NothingDue)
    );
}

/// A PROVEN ACTIVE ACCOUNT THAT IS LOW is not followed for its own sake: a
/// session on a fresh account would be relaunched onto a nearly spent one,
/// and rule 4 would then move it off again — two relaunches for nothing.
#[test]
fn a_low_active_account_is_not_followed_and_is_rotated_past_for_a_fresh_one() {
    let r = three();
    let enrolled = all(&r);
    let mut s = on_main();
    s.active = Some("work-2".into());
    low(&mut s, "work-2");
    fresh(&mut s, "main");
    let ask = |s: &State, who: &str, point: &Point, mode: Mode| {
        advise(&r, s, &enrolled, &me(who), point, mode, NOW)
    };
    let rotate = |to: &str, ahead: bool| Advice::Move {
        to: to.into(),
        ahead,
        rotates: true,
        takes_canary: false,
    };
    let follow = |ahead: bool| Advice::Move {
        to: "work-2".into(),
        ahead,
        rotates: false,
        takes_canary: false,
    };

    // At a plain turn end on a healthy account: stay.
    for mode in [Mode::Early, Mode::Wall] {
        assert_eq!(
            ask(&s, "main", &Point::TurnEnd, mode),
            Advice::Stay(StayWhy::NothingDue),
            "{}",
            mode.as_str()
        );
    }
    // NEGATIVE CONTROLS: the same account fresh is followed, and so is the
    // low one once it is the owner's pin.
    let mut healthy = s.clone();
    healthy.accounts.remove("work-2");
    assert_eq!(
        ask(&healthy, "main", &Point::TurnEnd, Mode::Early),
        follow(true)
    );
    let mut pinned = s.clone();
    pinned.pinned = true;
    assert_eq!(
        ask(&pinned, "main", &Point::TurnEnd, Mode::Early),
        follow(true)
    );
    assert_eq!(ask(&pinned, "main", &wall(), Mode::Early), follow(false));

    // AT A WALL ON `me` a fresh account is taken in its place, as a
    // rotation, and the compare-and-set lets it through.
    let mut at_wall = s.clone();
    limited(&mut at_wall, "main", NOW + 3 * HOUR);
    assert_eq!(
        ask(&at_wall, "main", &wall(), Mode::Wall),
        rotate("pool-a", false)
    );
    let mut moved = at_wall.clone();
    let event = begin_rotation(
        &mut moved,
        &r,
        &enrolled,
        Some("work-2"),
        mover("main"),
        "pool-a",
        NOW,
    )
    .expect("past a low active account, to a fresh one");
    assert_eq!(
        (event.account.as_str(), event.to.as_deref()),
        ("work-2", Some("pool-a"))
    );
    assert_eq!(
        (
            moved.active.as_deref(),
            moved.previous.as_deref(),
            moved.previous_proven,
            moved.proven
        ),
        (Some("pool-a"), Some("work-2"), true, false)
    );
    // …and at `me`'s own low turn end.
    let mut own_low = s.clone();
    own_low.accounts.remove("main");
    low(&mut own_low, "main");
    assert_eq!(
        ask(&own_low, "main", &Point::TurnEnd, Mode::Early),
        rotate("pool-a", true)
    );
    assert!(
        begin_rotation(
            &mut own_low.clone(),
            &r,
            &enrolled,
            Some("work-2"),
            mover("main"),
            "pool-a",
            NOW
        )
        .is_ok()
    );

    // THE HOUR'S ROTATIONS SPENT, the low active account is still there to
    // move onto — a follow, which rotates nothing — rather than wait the
    // wall out beside it.
    let spent = State {
        rotations: vec![NOW - 5; 6],
        ..at_wall.clone()
    };
    assert_eq!(ask(&spent, "main", &wall(), Mode::Wall), follow(false));
    // NEGATIVE CONTROLS: with no low active account to fall back on the
    // breaker holds — one that is limited, and one that is no candidate
    // because another account is about to open.
    let mut nowhere = spent.clone();
    limited(&mut nowhere, "work-2", NOW + 2 * HOUR);
    assert!(matches!(
        ask(&nowhere, "main", &wall(), Mode::Wall),
        Advice::Stay(StayWhy::Breaker { .. })
    ));
    let mut near = spent.clone();
    near.accounts.remove("main");
    limited(&mut near, "main", NOW + 5 * MIN);
    assert!(matches!(
        ask(&near, "main", &wall(), Mode::Wall),
        Advice::Stay(StayWhy::Breaker { .. })
    ));
    // Ahead of a wall the spent bound is a stay: one low account is not
    // left for another.
    let own_spent = State {
        rotations: vec![NOW - 5; 6],
        ..own_low.clone()
    };
    assert!(matches!(
        ask(&own_spent, "main", &Point::TurnEnd, Mode::Early),
        Advice::Stay(StayWhy::Breaker { .. })
    ));

    // WITH ONLY LOW ACCOUNTS LEFT the wall falls back on the active one —
    // where the other sessions are — as a follow: no rotation, so no
    // compare-and-set and no count toward the bound.
    let mut all_low = at_wall.clone();
    low(&mut all_low, "pool-a");
    assert_eq!(ask(&all_low, "main", &wall(), Mode::Wall), follow(false));
    all_low.rotations = vec![NOW - 5; 6];
    assert_eq!(
        ask(&all_low, "main", &wall(), Mode::Wall),
        follow(false),
        "a follow is not the breaker's"
    );
    // The compare-and-set agrees: a low account is not rotated past for
    // another low one, nor a FRESH one for anything.
    let held = Err(Lost::Held {
        active: "work-2".into(),
    });
    let mut all_low = State {
        rotations: Vec::new(),
        ..all_low
    };
    let before = all_low.clone();
    assert_eq!(
        begin_rotation(
            &mut all_low,
            &r,
            &enrolled,
            Some("work-2"),
            mover("main"),
            "pool-a",
            NOW
        ),
        held
    );
    assert_eq!(all_low, before);
    let mut healthy = at_wall.clone();
    healthy.accounts.remove("work-2");
    assert_eq!(
        begin_rotation(
            &mut healthy,
            &r,
            &enrolled,
            Some("work-2"),
            mover("main"),
            "pool-a",
            NOW
        ),
        held
    );
    // The pin holds a low active account in place.
    let mut pinned = at_wall.clone();
    pinned.pinned = true;
    assert_eq!(
        begin_rotation(
            &mut pinned,
            &r,
            &enrolled,
            Some("work-2"),
            mover("main"),
            "pool-a",
            NOW
        ),
        held
    );
}

#[test]
fn rule_3_at_a_wall_moves_to_the_next_candidate_or_waits_for_the_roster() {
    let r = three();
    let enrolled = all(&r);
    let mut s = on_main();
    limited(&mut s, "main", NOW + 3 * HOUR);
    let ask = |s: &State, mode: Mode| advise(&r, s, &enrolled, &me("main"), &wall(), mode, NOW);
    let to = |label: &str| Advice::Move {
        to: label.into(),
        ahead: false,
        rotates: true,
        takes_canary: false,
    };

    assert_eq!(ask(&s, Mode::Wall), to("work-2"));
    assert_eq!(
        ask(&s, Mode::Early),
        to("work-2"),
        "the wall moves in either mode"
    );
    limited(&mut s, "work-2", NOW + 2 * HOUR);
    assert_eq!(ask(&s, Mode::Wall), to("pool-a"));

    // None left: wait for the earliest instant any account is usable.
    limited(&mut s, "pool-a", NOW + 4 * HOUR);
    assert_eq!(
        ask(&s, Mode::Wall),
        Advice::Wait {
            until: NOW + 2 * HOUR
        }
    );
    // NEGATIVE CONTROL: a plain turn end on a limited roster waits for
    // nothing.
    assert_eq!(
        advise(
            &r,
            &s,
            &enrolled,
            &me("main"),
            &Point::TurnEnd,
            Mode::Wall,
            NOW
        ),
        Advice::Stay(StayWhy::NothingDue)
    );

    // A wall the state does not hold (a screen-only one is recorded for
    // nobody) with nothing to move to: no instant of the roster's is ahead,
    // so the wall is waited out as with no roster.
    let mut alone = on_main();
    note_auth(&mut alone, "work-2", Auth::SignedOut, NOW);
    let mut only_main = enrolled.clone();
    only_main.remove("pool-a");
    assert_eq!(
        advise(
            &r,
            &alone,
            &only_main,
            &me("main"),
            &wall(),
            Mode::Wall,
            NOW
        ),
        Advice::Stay(StayWhy::NoCandidate)
    );
    // …and with this session's own wall recorded, it waits for that.
    limited(&mut alone, "main", NOW + HOUR);
    assert_eq!(
        advise(
            &r,
            &alone,
            &only_main,
            &me("main"),
            &wall(),
            Mode::Wall,
            NOW
        ),
        Advice::Wait { until: NOW + HOUR }
    );
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

    // Fresh before low: a fresh SHARED account goes before a low dedicated
    // one.
    let mut s = state();
    low(&mut s, "work-2");
    fresh(&mut s, "pool-a");
    assert_eq!(order(&r, &s, Some("main"), &wall()), ["pool-a", "work-2"]);
    // NEGATIVE CONTROL: with both fresh the class decides again (above).

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

    // Unseen counts as fresh: an account nobody has observed goes before a
    // low one, and a dedicated unseen one before a shared usable one.
    let r = three();
    let mut s = state();
    fresh(&mut s, "pool-a");
    assert_eq!(order(&r, &s, Some("main"), &wall()), ["work-2", "pool-a"]);
    let mut s = state();
    low(&mut s, "work-2");
    assert_eq!(order(&r, &s, Some("main"), &wall()), ["pool-a", "work-2"]);

    // The whole list comes back: the candidates best first, then every
    // other row in roster order, each with its reason.
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

    // AUTH: only a login KNOWN signed out is skipped. One nobody has
    // checked is proposed (the move onto it is its check: a login wall
    // after it strikes it signed out), and so is one a check said works.
    let enrolled = all(&r);
    let mut s = state();
    assert_eq!(s.account("work-2").auth, Auth::Unseen);
    assert_eq!(order(&r, &s, Some("main"), &wall()), ["work-2", "pool-a"]);
    note_auth(&mut s, "work-2", Auth::Ok, NOW);
    assert_eq!(order(&r, &s, Some("main"), &wall()), ["work-2", "pool-a"]);
    note_auth(&mut s, "work-2", Auth::SignedOut, NOW + 1);
    assert_eq!(order(&r, &s, Some("main"), &wall()), ["pool-a"]);

    // LOW: a candidate only at a wall…
    let mut s = state();
    low(&mut s, "work-2");
    assert_eq!(
        why_not(&r, &s, &enrolled, &Point::TurnEnd, "work-2"),
        WhyNot::LowAheadOfAWall
    );
    assert_eq!(order(&r, &s, Some("main"), &Point::TurnEnd), ["pool-a"]);
    assert_eq!(order(&r, &s, Some("main"), &wall()), ["pool-a", "work-2"]);
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
    assert_eq!(LOW_ONLY_AFTER_S, 900);

    for (why, word) in [
        (WhyNot::Me, "this session's own"),
        (WhyNot::NotEnrolled, "not-enrolled"),
        (WhyNot::SignedOut, "signed-out"),
        (WhyNot::Limited { until: 0 }, "limited"),
        (WhyNot::LowAheadOfAWall, "low: used only at a wall"),
        (
            WhyNot::LowWhileOneIsNear { at: 0 },
            "low: an account is usable within 15 minutes",
        ),
    ] {
        assert_eq!(why.as_str(), word);
    }
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
    assert_eq!(DEDICATED_WAIT_S, 900);

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

#[test]
fn rule_4_moves_ahead_of_the_wall_only_from_a_low_account_to_a_fresh_one() {
    let r = three();
    let enrolled = all(&r);
    let ask =
        |s: &State, mode: Mode| advise(&r, s, &enrolled, &me("main"), &Point::TurnEnd, mode, NOW);
    let ahead = |label: &str| Advice::Move {
        to: label.into(),
        ahead: true,
        rotates: true,
        takes_canary: false,
    };
    let mut s = on_main();
    low(&mut s, "main");

    assert_eq!(ask(&s, Mode::Early), ahead("work-2"));
    // NEGATIVE CONTROLS: `wall` mode never moves early; an account that is
    // not low stays; a mode that is off stays.
    assert_eq!(ask(&s, Mode::Wall), Advice::Stay(StayWhy::NothingDue));
    assert_eq!(ask(&s, Mode::Off), Advice::Stay(StayWhy::Off));
    let mut healthy = on_main();
    fresh(&mut healthy, "main");
    assert_eq!(
        ask(&healthy, Mode::Early),
        Advice::Stay(StayWhy::NothingDue)
    );

    // A low account is no early candidate: only fresh ones are moved to
    // ahead of a wall.
    let mut s2 = s.clone();
    low(&mut s2, "work-2");
    assert_eq!(ask(&s2, Mode::Early), ahead("pool-a"));
    low(&mut s2, "pool-a");
    assert_eq!(ask(&s2, Mode::Early), Advice::Stay(StayWhy::NoFresh));

    // An account holding a reset offer about to expire is NOT left early:
    // the reset is offered only at a wall.
    assert_eq!(OFFER_EXPIRING_S, 86_400);
    let offered = |left: u32, use_by: Option<i64>| {
        let mut s = s.clone();
        assert!(note_offer(&mut s, "main", left, use_by, NOW - 5, NOW));
        ask(&s, Mode::Early)
    };
    assert_eq!(
        offered(1, Some(NOW + DAY)),
        Advice::Stay(StayWhy::OfferExpiring)
    );
    assert_eq!(
        offered(1, Some(NOW + 1)),
        Advice::Stay(StayWhy::OfferExpiring)
    );
    assert_eq!(
        offered(1, Some(NOW + DAY + 1)),
        ahead("work-2"),
        "more than a day off"
    );
    assert_eq!(offered(1, Some(NOW)), ahead("work-2"), "already expired");
    assert_eq!(
        offered(0, Some(NOW + HOUR)),
        ahead("work-2"),
        "no reset left"
    );
    assert_eq!(
        offered(1, None),
        ahead("work-2"),
        "a deadline nobody read is not one"
    );
}

/// WHO IS LEFT EARLY. The 15-minute guard is about leaving: a session is not
/// moved off an account whose low window is about to reset. And an account a
/// wall already stands on (another tab hit it, or a model's bucket) is left
/// as a low one is: `limited` must not mask `low`, or the account in the
/// WORSE condition is the one not left.
#[test]
fn rule_4_leaves_a_limited_account_and_not_one_whose_low_window_is_about_to_reset() {
    let r = three();
    let enrolled = all(&r);
    let ask =
        |s: &State, mode: Mode| advise(&r, s, &enrolled, &me("main"), &Point::TurnEnd, mode, NOW);
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
    assert_eq!(ask(&resets(NOW + RESET_SOON_S + 1), Mode::Early), ahead);
    assert_eq!(
        ask(&resets(NOW + RESET_SOON_S), Mode::Early),
        Advice::Stay(StayWhy::NothingDue)
    );

    // A wall recorded on a low account: left, as it was without the wall.
    let mut s = on_main();
    low(&mut s, "main");
    assert_eq!(ask(&s, Mode::Early), ahead);
    limited(&mut s, "main", NOW + 3 * HOUR);
    assert!(matches!(
        cond(&r, &s, "main", NOW),
        Condition::Limited { .. }
    ));
    assert_eq!(ask(&s, Mode::Early), ahead);
    // …and the compare-and-set takes it.
    assert!(
        begin_rotation(
            &mut s.clone(),
            &r,
            &enrolled,
            Some("main"),
            mover("main"),
            "work-2",
            NOW
        )
        .is_ok()
    );
    // A wall with no sample at all: left too, and no `/usage` is typed
    // first — the wall says more than a sample would.
    let mut bare = on_main();
    limited(&mut bare, "main", NOW + 3 * HOUR);
    assert_eq!(ask(&bare, Mode::Early), ahead);
    // NEGATIVE CONTROL: the same account with no wall is probed and stays.
    assert_eq!(ask(&on_main(), Mode::Early), Advice::Probe);

    // What holds a low account holds a limited one: the mode, the owner's
    // pin, a reset offer about to expire, no fresh account.
    assert_eq!(ask(&s, Mode::Wall), Advice::Stay(StayWhy::NothingDue));
    let mut pinned = s.clone();
    pinned.pinned = true;
    assert_eq!(ask(&pinned, Mode::Early), Advice::Stay(StayWhy::Pinned));
    let mut offered = s.clone();
    assert!(note_offer(
        &mut offered,
        "main",
        1,
        Some(NOW + HOUR),
        NOW,
        NOW
    ));
    assert_eq!(
        ask(&offered, Mode::Early),
        Advice::Stay(StayWhy::OfferExpiring)
    );
    let mut nowhere = s.clone();
    low(&mut nowhere, "work-2");
    low(&mut nowhere, "pool-a");
    assert_eq!(ask(&nowhere, Mode::Early), Advice::Stay(StayWhy::NoFresh));
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
    // Another account's probe is NOT its own any more: the vendor answers a
    // usage read from the home's snapshot for a minute, so no two accounts
    // are probed inside [`PROBE_APART_S`] (the spacing has its own test,
    // below).
    assert_eq!(
        claim_probe(&mut s, "work-2", "s-3", due + PROBE_MIN_S),
        None,
        "another account's claim in the same second"
    );

    // The probe goes before the early move: a low account whose sample is
    // stale is read again first.
    let mut s = on_main();
    low(&mut s, "main");
    assert_eq!(ask(&r, &s, Mode::Early, at + PROBE_MAX_S), Advice::Probe);
}

/// THE CLAIM'S SPACING ([`PROBE_APART_S`], [`probe_held`]). The vendor
/// answers a usage read from the home's snapshot while that is under 60 s
/// old (READ in the 2.1.287 bundle), whichever login fetched it, so no
/// account is probed within 90 s of ANY account's claim. NEGATIVE CONTROL:
/// the same account is held off for its own floor, twice as long — and the
/// ONE claim field keeps that floor through another account's claim, which
/// can write over this account's no sooner than 90 s after it.
#[test]
fn no_account_is_probed_within_ninety_seconds_of_any_accounts_claim() {
    assert_eq!((PROBE_APART_S, PROBE_MIN_S), (90, 180));
    assert_eq!(2 * PROBE_APART_S, PROBE_MIN_S, "what keeps the floor");
    let t = NOW;
    let claimed = || {
        let mut s = on_main();
        assert!(claim_probe(&mut s, "work-2", "s-two", t).is_some());
        s
    };
    // Another account: refused at t+89, taken at t+90.
    let mut s = claimed();
    assert!(probe_held(&s, "main", t + PROBE_APART_S - 1));
    assert_eq!(claim_probe(&mut s, "main", "s-one", t + 89), None);
    assert_eq!(
        s.probe.as_ref().map(|c| c.account.as_str()),
        Some("work-2"),
        "a refused claim writes nothing"
    );
    assert!(!probe_held(&s, "main", t + PROBE_APART_S));
    let taken = claim_probe(&mut s, "main", "s-one", t + 90).expect("taken at ninety");
    assert_eq!(
        (taken.kind, taken.account.as_str(), taken.sid.as_deref()),
        (Kind::Probe, "main", Some("s-one"))
    );
    // NEGATIVE CONTROL: the same account is still refused until t+180.
    let mut s = claimed();
    for dt in [0, 89, 90, 179] {
        assert!(probe_held(&s, "work-2", t + dt), "+{dt}");
        assert_eq!(
            claim_probe(&mut s, "work-2", "s-three", t + dt),
            None,
            "+{dt}"
        );
    }
    assert!(claim_probe(&mut s, "work-2", "s-three", t + PROBE_MIN_S).is_some());

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
    // NEGATIVE CONTROL of the rule's reach: a session on ANOTHER account
    // than the unproven active is held by the canary before rule 4 is
    // reached at all, as it was.
    assert_eq!(
        ask(&unproven(&bare), "main", NOW),
        Advice::Stay(StayWhy::Canary)
    );
}

/// The fixtures [`advise_past_probe`] is swept over: every shape the advice
/// tests above build by hand, crossed. Each is `(what, state)`.
fn probe_sweep_states() -> Vec<(String, State)> {
    let r = three();
    let enrolled = all(&r);
    let mut out: Vec<(String, State)> = Vec::new();
    /// Puts an account in a condition, by the real transactions.
    type Put = fn(&mut State, &str);
    let conditions: [(&str, Put); 4] = [
        ("unseen", |_, _| {}),
        ("fresh", |s, l| fresh(s, l)),
        ("low", |s, l| low(s, l)),
        ("limited", |s, l| limited(s, l, NOW + 3 * HOUR)),
    ];
    for (main_is, on_main_do) in conditions {
        for (two_is, on_two_do) in conditions {
            for (pool_is, on_pool_do) in conditions {
                let mut base = state();
                on_main_do(&mut base, "main");
                on_two_do(&mut base, "work-2");
                on_pool_do(&mut base, "pool-a");
                let what = format!("main {main_is}, work-2 {two_is}, pool-a {pool_is}");
                // Never rotated; `main` active and proven; `work-2` active,
                // proven or not, with a canary whose hold stands or lapsed.
                out.push((format!("{what}; no active"), base.clone()));
                for active in ["main", "work-2"] {
                    out.push((
                        format!("{what}; {active} proven"),
                        State {
                            active: Some(active.to_string()),
                            proven: true,
                            ..base.clone()
                        },
                    ));
                    for (hold, at) in [("held", NOW - 30), ("lapsed", NOW - CANARY_HOLD_S - 1)] {
                        out.push((
                            format!("{what}; {active} unproven, {hold}"),
                            State {
                                active: Some(active.to_string()),
                                proven: false,
                                canary: Some(Canary {
                                    sid: "s-can".to_string(),
                                    at,
                                    took: false,
                                }),
                                ..base.clone()
                            },
                        ));
                    }
                }
            }
        }
    }
    // The owner's pin, a reset offer about to expire, the hour's rotations
    // spent, and each kind of claim.
    let mut extra: Vec<(String, State)> = Vec::new();
    for (what, s) in &out {
        if !what.ends_with("main proven") {
            continue;
        }
        let mut pinned = s.clone();
        if pin(&mut pinned, &r, &enrolled, "main", NOW).is_ok() {
            extra.push((format!("{what}; pinned"), pinned));
        }
        let mut offered = s.clone();
        if note_offer(&mut offered, "main", 1, Some(NOW + HOUR), NOW, NOW) {
            extra.push((format!("{what}; offer"), offered));
        }
        extra.push((
            format!("{what}; breaker"),
            State {
                rotations: vec![NOW - 10; 2 * r.rows.len()],
                ..s.clone()
            },
        ));
        for (whose, age) in [("main", 10), ("main", PROBE_MIN_S), ("pool-a", 10)] {
            extra.push((
                format!("{what}; claim {whose} {age}s"),
                State {
                    probe: Some(ProbeClaim {
                        account: whose.to_string(),
                        sid: "s-claim".to_string(),
                        at: NOW - age,
                    }),
                    ..s.clone()
                },
            ));
        }
    }
    out.extend(extra);
    out
}

/// [`advise_past_probe`] IS [`advise`] WITH THE PROBE'S ARM SKIPPED: never
/// `Probe`; the same answer wherever [`advise`] is not `Probe`; and where it
/// is, the answer [`advise`] gives with the probe held off (this account's
/// own claim, a second old) — so "past the probe" is exactly "as if someone
/// were already making it". Swept over every fixture shape of this file:
/// three accounts each unseen, fresh, low or limited; no active account,
/// each of two active and proven, or unproven under a hold or a lapsed one;
/// the pin, an offer, the breaker, a claim; both sessions; a plain turn end
/// and a wall, dated and not; the three modes; a roster of one and a
/// session the roster does not list; two clocks. NEGATIVE CONTROL: the
/// sweep meets the probe, and meets a MOVE behind it — the move a due probe
/// hid (counted, so a sweep that never reached rule 4 could not pass).
#[test]
fn advise_past_probe_is_advise_with_the_probes_arm_skipped() {
    let r = three();
    let enrolled = all(&r);
    let alone = roster(&[("main", Class::Dedicated, None)]);
    let points = [Point::TurnEnd, wall(), wall_at(NOW - 5 * MIN)];
    let (mut asked, mut probes, mut moves_behind, mut stays_behind) = (0, 0, 0, 0);
    for (what, s) in probe_sweep_states() {
        for label in ["main", "work-2", "elsewhere"] {
            for point in &points {
                for mode in [Mode::Early, Mode::Wall, Mode::Off] {
                    for roster in [&r, &alone] {
                        for now in [NOW, NOW - MIN + PROBE_MAX_S] {
                            let ask = |s: &State, past: bool| {
                                let f = if past { advise_past_probe } else { advise };
                                f(roster, s, &enrolled, &me(label), point, mode, now)
                            };
                            let (plain, past) = (ask(&s, false), ask(&s, true));
                            let at = format!("{what}; {label} at {point:?}, {mode:?}, {now}");
                            asked += 1;
                            assert_ne!(past, Advice::Probe, "{at}");
                            if plain != Advice::Probe {
                                assert_eq!(past, plain, "{at}");
                                continue;
                            }
                            probes += 1;
                            let held = State {
                                probe: Some(ProbeClaim {
                                    account: label.to_string(),
                                    sid: "s-held".to_string(),
                                    at: now - 1,
                                }),
                                ..s.clone()
                            };
                            assert_eq!(past, ask(&held, false), "{at}");
                            match past {
                                Advice::Move { ahead, .. } => {
                                    assert!(ahead, "{at}");
                                    moves_behind += 1;
                                }
                                Advice::Stay(_) => stays_behind += 1,
                                other => panic!("{at}: behind the probe, {other:?}"),
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(asked > 10_000, "{asked}");
    assert!(
        probes > 0 && moves_behind > 0 && stays_behind > 0,
        "{probes} probes, {moves_behind} moves and {stays_behind} stays behind them"
    );
}

#[test]
fn the_probe_interval_is_half_the_time_to_the_threshold_clamped() {
    assert_eq!(
        (PROBE_MIN_S, PROBE_MAX_S, PROBE_DEFAULT_S),
        (180, 1200, 600)
    );
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

#[test]
fn the_owners_pin_turns_the_early_move_off_and_everyone_follows() {
    let r = three();
    let enrolled = all(&r);
    let mut s = on_main();
    low(&mut s, "pool-a");
    limited(&mut s, "pool-a", NOW + HOUR);

    // `use pool-a`: its walls are cleared, and it is active, proven, pinned.
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

    // Rule 2: every session moves at its next point, a healthy one too.
    for point in [Point::TurnEnd, wall()] {
        assert_eq!(
            advise(&r, &s, &enrolled, &me("main"), &point, Mode::Wall, NOW),
            Advice::Move {
                to: "pool-a".into(),
                ahead: point == Point::TurnEnd,
                rotates: false,
                takes_canary: false
            }
        );
    }
    // Rule 4 is off: the pinned account is low, fresh ones exist, and the
    // session on it stays — with no probe typed either.
    let on_pin = |s: &State| {
        advise(
            &r,
            s,
            &enrolled,
            &me("pool-a"),
            &Point::TurnEnd,
            Mode::Early,
            NOW,
        )
    };
    assert_eq!(on_pin(&s), Advice::Stay(StayWhy::Pinned));
    // NEGATIVE CONTROL: the same state unpinned moves away.
    let mut auto = s.clone();
    let row = unpin(&mut auto, "use --auto", NOW).expect("a pin held");
    assert_eq!(
        (row.kind, row.account.as_str(), row.why.as_str()),
        (Kind::Unpin, "pool-a", "use --auto")
    );
    assert_eq!(unpin(&mut auto, "use --auto", NOW), None, "no pin, no row");
    assert_eq!(
        on_pin(&auto),
        Advice::Move {
            to: "main".into(),
            ahead: true,
            rotates: true,
            takes_canary: false
        }
    );
    // The compare-and-set holds the pin too.
    assert_eq!(
        begin_rotation(
            &mut s,
            &r,
            &enrolled,
            Some("pool-a"),
            mover("pool-a"),
            "main",
            NOW
        ),
        Err(Lost::Pinned)
    );

    // A wall on the pinned account ends the pin, in the ledger, and
    // rotation resumes.
    let events = note_wall(&mut s, "pool-a", "seven_day", Some(NOW + DAY), NOW, NOW);
    assert_eq!(
        events
            .iter()
            .map(|e| (e.kind, e.why.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                Kind::Wall,
                format!("seven_day until {}", NOW + DAY).as_str()
            ),
            (Kind::Unpin, "wall")
        ]
    );
    assert!(!s.pinned);
    assert_eq!(
        advise(&r, &s, &enrolled, &me("pool-a"), &wall(), Mode::Early, NOW),
        Advice::Move {
            to: "main".into(),
            ahead: false,
            rotates: true,
            takes_canary: false
        }
    );
    // A wall on ANOTHER account leaves a pin alone.
    let mut s = on_main();
    pin(&mut s, &r, &enrolled, "main", NOW).expect("pins");
    assert_eq!(
        note_wall(&mut s, "work-2", "five_hour", Some(NOW + HOUR), NOW, NOW).len(),
        1
    );
    assert!(s.pinned);
    // Found signed out, the pinned account's pin ends.
    let events = note_auth(&mut s, "main", Auth::SignedOut, NOW);
    assert_eq!(
        events.iter().map(|e| e.kind).collect::<Vec<_>>(),
        [Kind::Auth, Kind::Unpin]
    );
    assert!(!s.pinned);

    // Refused by name, and nothing changes: not listed, not enrolled,
    // signed out.
    let mut s = on_main();
    note_auth(&mut s, "work-2", Auth::SignedOut, NOW);
    let before = s.clone();
    let mut here = enrolled.clone();
    here.remove("pool-a");
    for (label, needle) in [
        ("nobody", "`nobody` is not in the roster"),
        ("pool-a", "`pool-a` is not-enrolled"),
        ("work-2", "`work-2` is signed-out"),
    ] {
        let err = pin(&mut s, &r, &here, label, NOW).expect_err(needle);
        assert!(err.contains(needle), "{err}");
    }
    assert_eq!(s, before);
}

/// THE OWNER LIFTED THIS WALL (`Advice::Resume`, between rules 2 and 3). A
/// session stands at its account's limit; the owner says `accounts use` for
/// that account, which clears its walls and pins it. The session's row is
/// older than the clearing, so the advice is to continue here, once for
/// that clearing — and it commits nothing.
///
/// Before, rule 3 advised the rotation at that row: the recorder dropped
/// the row (cleared) and the compare-and-set refused the rotation (pinned),
/// so the session was told to move at every look and moved at none, until
/// the row's own reset.
#[test]
fn the_owners_clearing_resumes_a_session_at_the_wall_it_lifted() {
    let r = three();
    let enrolled = all(&r);
    let written = NOW - HOUR;
    let used = NOW - MIN;
    let until = Some(NOW + 3 * HOUR);
    let ask = |s: &State, me: &Me<'_>, point: &Point| {
        advise(&r, s, &enrolled, me, point, Mode::Wall, NOW)
    };
    let rotate = Advice::Move {
        to: "work-2".into(),
        ahead: false,
        rotates: true,
        takes_canary: false,
    };

    // The session at main's wall, the wall on record.
    let mut walled = on_main();
    assert_eq!(
        note_wall(&mut walled, "main", "five_hour", until, written, written).len(),
        1
    );
    // NEGATIVE CONTROL: nobody cleared it. Rule 3.
    assert_eq!(ask(&walled, &me("main"), &wall_at(written)), rotate);

    // The owner's word.
    let mut s = walled.clone();
    pin(&mut s, &r, &enrolled, "main", used).expect("pins");
    let resume = Advice::Resume { cleared_at: used };
    assert_eq!(ask(&s, &me("main"), &wall_at(written)), resume);
    assert_eq!(
        advise(
            &r,
            &s,
            &enrolled,
            &me("main"),
            &wall_at(written),
            Mode::Early,
            NOW
        ),
        resume,
        "under either mode: it is a wall's advice"
    );
    // IT COMMITS NOTHING, and the transactions say the same: the row is the
    // wall the owner cleared, and the pin forbids the rotation.
    let before = s.clone();
    assert!(note_wall(&mut s, "main", "five_hour", until, written, NOW).is_empty());
    let commit = |s: &State| {
        begin_rotation(
            &mut s.clone(),
            &r,
            &enrolled,
            Some("main"),
            mover("main"),
            "work-2",
            NOW,
        )
    };
    assert_eq!(commit(&s), Err(Lost::Pinned));
    assert_eq!(s, before);

    // NEGATIVE CONTROLS, each one edit away.
    // A row nobody dated says nothing of the clearing: rule 3, as before.
    assert_eq!(ask(&s, &me("main"), &wall()), rotate);
    // A row of the clearing's own second, or after it, is a wall met since.
    for met in [used, used + 1, NOW] {
        assert_eq!(ask(&s, &me("main"), &wall_at(met)), rotate, "{met}");
    }
    // Not at a wall, and with the mode off, nothing is resumed.
    assert_eq!(
        ask(&s, &me("main"), &Point::TurnEnd),
        Advice::Stay(StayWhy::NothingDue)
    );
    assert_eq!(
        advise(
            &r,
            &s,
            &enrolled,
            &me("main"),
            &wall_at(written),
            Mode::Off,
            NOW
        ),
        Advice::Stay(StayWhy::Off)
    );
    // Another account's clearing is not this one's: a session at work-2's
    // old wall follows the pin (rule 2).
    assert_eq!(
        ask(&s, &me("work-2"), &wall_at(written)),
        Advice::Move {
            to: "main".into(),
            ahead: false,
            rotates: false,
            takes_canary: false
        }
    );

    // THE ACCOUNT ANSWERS WITH A NEW WALL: an ordinary one. Its row is dated
    // after the clearing, so it is recorded, the pin ends, and the rotation
    // the advice gives commits.
    let met = NOW - 30;
    let mut again = s.clone();
    let rows = note_wall(&mut again, "main", "five_hour", until, met, NOW);
    assert_eq!(
        rows.iter()
            .map(|e| (e.kind, e.why.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                Kind::Wall,
                format!("five_hour until {}", NOW + 3 * HOUR).as_str()
            ),
            (Kind::Unpin, "wall")
        ]
    );
    assert_eq!(ask(&again, &me("main"), &wall_at(met)), rotate);
    assert!(commit(&again).is_ok());
    // …and a second session still standing at the OLD row is not resumed
    // onto an account a wall stands on again.
    assert_eq!(ask(&again, &me("main"), &wall_at(written)), rotate);

    // THE CLEARING IS WHAT IS ASKED, not the pin: `use --auto` after it
    // leaves the wall lifted.
    let mut auto = s.clone();
    unpin(&mut auto, "use --auto", NOW).expect("a pin held");
    assert_eq!(ask(&auto, &me("main"), &wall_at(written)), resume);

    // RULE 2 COMES FIRST. The owner's later word for another account is
    // followed…
    let mut moved_on = s.clone();
    pin(&mut moved_on, &r, &enrolled, "work-2", NOW - 30).expect("pins");
    assert_eq!(
        ask(&moved_on, &me("main"), &wall_at(written)),
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
        ask(&unanswered, &me("main"), &wall_at(written)),
        Advice::Stay(StayWhy::Canary)
    );

    // A login known signed out is not continued on.
    let mut out = s.clone();
    note_auth(&mut out, "main", Auth::SignedOut, NOW);
    assert_eq!(ask(&out, &me("main"), &wall_at(written)), rotate);

    // A CLEARING STAMPED AHEAD OF THE CLOCK is read as made now, and its
    // key with it (stated on `advise`: such a key moves with the clock).
    let mut ahead = s.clone();
    ahead.accounts.get_mut("main").expect("cleared").cleared_at = Some(NOW + DAY);
    assert_eq!(
        ask(&ahead, &me("main"), &wall_at(written)),
        Advice::Resume { cleared_at: NOW }
    );
}

/// THE TRANSACTIONS AGREE WITH THE ADVICE AT A DATED ROW, over every small
/// state where the owner has cleared `main`: three accounts each never seen,
/// fresh, low or limited; `main` or `work-2` active and proven, pinned or
/// not; the hour's rotations spent or not; the session on `main` at a row
/// written before the clearing, and at one written after it.
///
/// A `Resume` is advised only at a row older than the clearing, and the
/// host's commit writes NOTHING for it (the row is the wall the owner
/// cleared). Every rotation advised beside it still commits on the state
/// with the row recorded — the pin never refuses one.
#[test]
fn a_resume_commits_nothing_and_every_rotation_beside_it_still_commits() {
    let r = three();
    let enrolled = all(&r);
    let labels = ["main", "work-2", "pool-a"];
    let (before, used, after) = (NOW - HOUR, NOW - 2 * MIN, NOW - 30);
    let until = Some(NOW + 4 * HOUR);
    let (mut resumed, mut rotated, mut under_a_pin) = (0, 0, 0);
    for conds in 0..64_usize {
        let mut base = state();
        for (k, label) in labels.iter().enumerate() {
            match (conds >> (2 * k)) & 3 {
                0 => {}
                1 => fresh(&mut base, label),
                2 => low(&mut base, label),
                // A wall met a minute ago: after the clearing.
                _ => limited(&mut base, label, NOW + 3 * HOUR),
            }
        }
        base.accounts.entry("main".into()).or_default().cleared_at = Some(used);
        for active in ["main", "work-2"] {
            for pinned in [false, true] {
                for spent in [false, true] {
                    let s = State {
                        active: Some(active.to_string()),
                        proven: true,
                        pinned,
                        rotations: vec![NOW - MIN; if spent { 2 * labels.len() } else { 0 }],
                        ..base.clone()
                    };
                    for written in [before, after] {
                        let point = wall_at(written);
                        let ask = |s: &State| {
                            advise(&r, s, &enrolled, &me("main"), &point, Mode::Wall, NOW)
                        };
                        // The state the host commits on: this row noted.
                        let mut recorded = s.clone();
                        note_wall(&mut recorded, "main", "five_hour", until, written, NOW);
                        match ask(&s) {
                            Advice::Resume { cleared_at } => {
                                resumed += 1;
                                assert_eq!((written, cleared_at), (before, used), "{s:?}");
                                assert_eq!(recorded, s, "nothing is written for it");
                                assert_eq!(ask(&recorded), ask(&s));
                                under_a_pin += usize::from(pinned);
                            }
                            Advice::Move {
                                to, rotates: true, ..
                            } => {
                                rotated += 1;
                                let won = begin_rotation(
                                    &mut recorded,
                                    &r,
                                    &enrolled,
                                    Some(active),
                                    mover("main"),
                                    &to,
                                    NOW,
                                );
                                assert!(won.is_ok(), "{won:?}: to {to} at {written} on {s:?}");
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    // Not vacuous: both were advised, a resume under the owner's pin too.
    assert!(resumed > 100, "{resumed}");
    assert!(rotated > 100, "{rotated}");
    assert!(under_a_pin > 0);
}

/// THE WAIT ENDS AT WHATEVER COMES FIRST. A session held at its wall — by a
/// canary's hold, or by the machine's rotation bound — waits for the end of
/// what holds it, or for the roster's next opening where that is sooner.
///
/// Held for the hour beside an active account whose own wall ended twenty
/// minutes in, the session followed it forty minutes late.
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
    // The wait's own rule: a dedicated account minutes behind a shared one.
    let mut near = on_main();
    limited(&mut near, "pool-a", NOW + 20 * MIN);
    limited(&mut near, "work-2", NOW + 30 * MIN);
    assert_eq!(opening(&near), Some(NOW + 30 * MIN));
    // A low account opens when the window that makes it low resets.
    let mut spent = on_main();
    let read = Sample {
        session_reset: Some(NOW + 10 * MIN),
        at: NOW,
        ..sample(95, 20)
    };
    assert_eq!(note_sample(&mut spent, "work-2", read, NOW).len(), 1);
    assert_eq!(opening(&spent), Some(NOW + 10 * MIN));

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

    // THE CASE IT IS FOR, through the real advice. The hour's rotations are
    // spent; the active account, proven, is limited for twenty more minutes;
    // the session on `main` is at its wall with a fresh candidate it may
    // not rotate to.
    let mut s = State {
        active: Some("work-2".into()),
        proven: true,
        rotations: vec![NOW - 10 * MIN; 2 * r.rows.len()],
        ..state()
    };
    limited(&mut s, "work-2", NOW + 20 * MIN);
    let at_wall =
        |s: &State, now: i64| advise(&r, s, &enrolled, &me("main"), &wall(), Mode::Wall, now);
    let Advice::Stay(why) = at_wall(&s, NOW) else {
        panic!("{:?}", at_wall(&s, NOW));
    };
    assert_eq!(
        why,
        StayWhy::Breaker {
            until: NOW + 50 * MIN
        }
    );
    let until = hold_until(why, &s, opening(&s), NOW).expect("a hold");
    assert_eq!(
        until,
        NOW + 20 * MIN,
        "the active account's wall ends first"
    );
    // At that instant the active account is followed: no rotation needed.
    assert_eq!(
        at_wall(&s, until),
        Advice::Move {
            to: "work-2".into(),
            ahead: false,
            rotates: false,
            takes_canary: false
        }
    );
}

// ---------------------------------------------------------------------------
// 5. Transactions
// ---------------------------------------------------------------------------

#[test]
fn two_sessions_at_one_wall_make_one_rotation_and_the_loser_follows_only_a_proven_account() {
    let r = three();
    let enrolled = all(&r);
    let mut s = on_main();
    limited(&mut s, "main", NOW + 3 * HOUR);
    let advice = |s: &State| advise(&r, s, &enrolled, &me("main"), &wall(), Mode::Early, NOW);
    let rotate = Advice::Move {
        to: "work-2".into(),
        ahead: false,
        rotates: true,
        takes_canary: false,
    };

    // Both sessions read the same state and get the same advice.
    let read = s.active.clone();
    assert_eq!(advice(&s), rotate);

    // The first commits: it moves ALONE.
    let won = begin_rotation(
        &mut s,
        &r,
        &enrolled,
        read.as_deref(),
        mover("main"),
        "work-2",
        NOW,
    )
    .expect("the first session wins");
    assert_eq!(
        (won.kind, won.account.as_str(), won.to.as_deref(), won.t),
        (Kind::Rotate, "main", Some("work-2"), NOW)
    );
    assert_eq!(
        (s.active.as_deref(), s.previous.as_deref(), s.proven),
        (Some("work-2"), Some("main"), false)
    );
    assert_eq!(s.rotations, [NOW], "it counts toward the machine's bound");

    assert_eq!(s.canary_since(), Some(NOW));
    // The canary itself is not held by its own rotation.
    assert_eq!(
        advise(
            &r,
            &s,
            &enrolled,
            &me("work-2"),
            &Point::TurnEnd,
            Mode::Wall,
            NOW
        ),
        Advice::Stay(StayWhy::NothingDue)
    );

    // The second commits what it read: lost, and nothing changes.
    let before = s.clone();
    assert_eq!(
        begin_rotation(
            &mut s,
            &r,
            &enrolled,
            read.as_deref(),
            mover("main"),
            "work-2",
            NOW
        ),
        Err(Lost::Raced {
            active: Some("work-2".into())
        })
    );
    assert_eq!(s, before);
    // Advised once more, it does not move: the candidate is not proven.
    assert_eq!(advice(&s), Advice::Stay(StayWhy::Canary));
    // …and it cannot rotate past the canary by asking for another account.
    assert_eq!(
        begin_rotation(
            &mut s,
            &r,
            &enrolled,
            Some("work-2"),
            mover("main"),
            "pool-a",
            NOW
        ),
        Err(Lost::Held {
            active: "work-2".into()
        })
    );
    assert_eq!(s, before);

    // The canary is served: proven, and only now does the loser follow.
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
    assert_eq!(
        advice(&s),
        Advice::Move {
            to: "work-2".into(),
            ahead: false,
            rotates: false,
            takes_canary: false
        }
    );
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
    assert!(
        begin_rotation(
            &mut s.clone(),
            &r,
            &enrolled,
            Some("main"),
            mover("main"),
            "work-2",
            NOW
        )
        .is_ok()
    );
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

    // An active account nobody is on: nothing, or one that cannot be run
    // on, is rotated past by any session.
    let mut s = state();
    assert!(begin_rotation(&mut s, &r, &enrolled, None, mover("main"), "work-2", NOW).is_ok());
    let mut s = on_main();
    limited(&mut s, "main", NOW + HOUR);
    let event = begin_rotation(
        &mut s,
        &r,
        &enrolled,
        Some("main"),
        mover("work-2"),
        "pool-a",
        NOW,
    )
    .expect("active as read cannot be run on");
    assert_eq!(
        (event.account.as_str(), event.to.as_deref()),
        ("main", Some("pool-a"))
    );
    // NEGATIVE CONTROL: the same call with `main` healthy is held.
    let mut s = on_main();
    assert_eq!(
        begin_rotation(
            &mut s,
            &r,
            &enrolled,
            Some("main"),
            mover("work-2"),
            "pool-a",
            NOW
        ),
        Err(Lost::Held {
            active: "main".into()
        })
    );
}

#[test]
fn a_struck_candidate_puts_active_back_and_nobody_else_was_moved() {
    let r = three();
    let enrolled = all(&r);
    let start = || {
        let mut s = on_main();
        limited(&mut s, "main", NOW + 3 * HOUR);
        begin_rotation(
            &mut s,
            &r,
            &enrolled,
            Some("main"),
            mover("main"),
            "work-2",
            NOW,
        )
        .expect("rotates");
        s
    };

    // It answered with a wall of its own: limited, and active goes back.
    let mut s = start();
    let events = strike(
        &mut s,
        "work-2",
        "s-one",
        &Strike::Wall {
            window: "five_hour".into(),
            until: Some(NOW + 2 * HOUR),
        },
        NOW + 20,
    );
    assert_eq!(
        events.iter().map(|e| e.kind).collect::<Vec<_>>(),
        [Kind::Struck, Kind::Wall]
    );
    assert_eq!(
        (
            events[0].account.as_str(),
            events[0].to.as_deref(),
            events[0].why.as_str()
        ),
        ("work-2", Some("main"), "wall: five_hour")
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
    // The session that never moved is advised the NEXT candidate, and the
    // canary itself (now on the struck account) the same.
    let next = Advice::Move {
        to: "pool-a".into(),
        ahead: false,
        rotates: true,
        takes_canary: false,
    };
    assert_eq!(
        advise(
            &r,
            &s,
            &enrolled,
            &me("main"),
            &wall(),
            Mode::Early,
            NOW + 20
        ),
        next
    );
    assert_eq!(
        advise(
            &r,
            &s,
            &enrolled,
            &me("work-2"),
            &wall(),
            Mode::Early,
            NOW + 20
        ),
        next
    );
    assert!(
        begin_rotation(
            &mut s,
            &r,
            &enrolled,
            Some("main"),
            mover("work-2"),
            "pool-a",
            NOW + 20
        )
        .is_ok()
    );

    // A login wall: signed out, never a candidate again until a check.
    let mut s = start();
    let events = strike(&mut s, "work-2", "s-one", &Strike::SignedOut, NOW + 20);
    assert_eq!(
        events.iter().map(|e| e.kind).collect::<Vec<_>>(),
        [Kind::Struck, Kind::Auth]
    );
    assert_eq!(s.active.as_deref(), Some("main"));
    assert_eq!(cond(&r, &s, "work-2", NOW + DAY), Condition::SignedOut);

    // A move that failed with no word about the account: struck on the
    // ladder of a wall naming no reset, so it is not chosen again at once.
    let mut s = start();
    let events = strike(
        &mut s,
        "work-2",
        "s-one",
        &Strike::Failed("no-resume".into()),
        NOW + 20,
    );
    assert_eq!(events[0].why, "failed: no-resume");
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

    // A move the host never made (the agent's jobs, a line that cannot
    // carry the login): active goes back, NOTHING is held against the
    // account, and the rotation is taken back out of the machine's bound.
    let mut s = start();
    assert_eq!(s.rotations.len(), 1);
    let events = strike(
        &mut s,
        "work-2",
        "s-one",
        &Strike::NotMoved("wait:changed".into()),
        NOW + 20,
    );
    assert_eq!(events.len(), 1);
    assert_eq!(
        (
            events[0].kind,
            events[0].to.as_deref(),
            events[0].why.as_str()
        ),
        (Kind::Struck, Some("main"), "not moved: wait:changed")
    );
    assert_eq!((s.active.as_deref(), s.proven), (Some("main"), true));
    assert!(
        s.rotations.is_empty(),
        "a rotation nobody made is not counted"
    );
    assert_eq!(
        cond(&r, &s, "work-2", NOW + 20),
        Condition::Unseen,
        "not blamed"
    );
    assert_eq!(
        advise(
            &r,
            &s,
            &enrolled,
            &me("main"),
            &wall(),
            Mode::Early,
            NOW + 20
        ),
        Advice::Move {
            to: "work-2".into(),
            ahead: false,
            rotates: true,
            takes_canary: false
        },
        "it is asked again at the next point"
    );
    // …and it says nothing about an account that is not the canary's.
    let mut s = start();
    let before = s.clone();
    assert!(
        strike(
            &mut s,
            "pool-a",
            "s-one",
            &Strike::NotMoved("wait:changed".into()),
            NOW
        )
        .is_empty()
    );
    prove(&mut s, "work-2", NOW + 10).expect("proven");
    let before_proven = s.clone();
    let late = Strike::NotMoved("late".into());
    assert!(strike(&mut s, "work-2", "s-one", &late, NOW + 20).is_empty());
    assert_eq!(s, before_proven);
    assert_ne!(before, before_proven);

    // NEGATIVE CONTROL: an account that is not the unproven active one is
    // marked, and `active` does not move.
    let mut s = start();
    prove(&mut s, "work-2", NOW + 10).expect("proven");
    strike(&mut s, "work-2", "s-one", &Strike::SignedOut, NOW + 20);
    assert_eq!((s.active.as_deref(), s.proven), (Some("work-2"), true));
    let mut s = start();
    strike(&mut s, "pool-a", "s-one", &Strike::SignedOut, NOW + 20);
    assert_eq!((s.active.as_deref(), s.proven), (Some("work-2"), false));

    // A first rotation from nothing, struck: active is nothing again.
    let mut s = state();
    begin_rotation(&mut s, &r, &enrolled, None, mover("main"), "work-2", NOW).expect("rotates");
    strike(&mut s, "work-2", "s-one", &Strike::SignedOut, NOW + 20);
    assert_eq!((s.active.as_deref(), s.proven), (None, false));
}

/// Found by the Tier-1 walk of `SupervisorAccountRotation`
/// (`tests/supervise_conformance_account_rotation.rs`): the session a
/// rotation moved was advised to rotate AGAIN, ahead of a wall, before the
/// account it had chosen served a response.
#[test]
fn the_session_that_chose_an_unproven_account_is_not_moved_on_before_it_answers() {
    let r = three();
    let both: BTreeSet<String> = ["main", "work-2"].map(str::to_string).into();
    let enrolled = all(&r);
    let mut s = state();
    limited(&mut s, "main", NOW + 3 * HOUR);
    low(&mut s, "work-2");

    // At the wall the low account is the only candidate, and it is taken.
    assert_eq!(
        advise(&r, &s, &both, &me("main"), &wall(), Mode::Early, NOW),
        Advice::Move {
            to: "work-2".into(),
            ahead: false,
            rotates: true,
            takes_canary: false
        }
    );
    begin_rotation(&mut s, &r, &both, None, mover("main"), "work-2", NOW).expect("rotates");
    assert_eq!((s.active.as_deref(), s.proven), (Some("work-2"), false));

    // A fresh account appears (its login is made). The session is on a low
    // account with a fresh candidate, which is rule 4's move — but its
    // account has not answered yet, so it stays, and the compare-and-set
    // refuses the same.
    let ahead = |s: &State| {
        advise(
            &r,
            s,
            &enrolled,
            &me("work-2"),
            &Point::TurnEnd,
            Mode::Early,
            NOW,
        )
    };
    assert_eq!(ahead(&s), Advice::Stay(StayWhy::Canary));
    let before = s.clone();
    assert_eq!(
        begin_rotation(
            &mut s,
            &r,
            &enrolled,
            Some("work-2"),
            mover("work-2"),
            "pool-a",
            NOW
        ),
        Err(Lost::Held {
            active: "work-2".into()
        })
    );
    assert_eq!(s, before, "a refusal writes nothing");

    // PROOF TRAVELS WITH THE ACCOUNT. The refusal above does not make an
    // unproven `previous` unreachable: a wall recorded on the unproven
    // account with `note_wall` (no strike) lets a rotation past it, and that
    // rotation leaves it behind. So what is left behind says whether it was
    // proven, and a strike of the next account restores EXACTLY that — the
    // account that never served a response comes back unproven, and is not
    // followed.
    let mut left = before.clone();
    assert_eq!(
        note_wall(
            &mut left,
            "work-2",
            "five_hour",
            Some(NOW + 2 * HOUR),
            NOW,
            NOW
        )
        .len(),
        1
    );
    begin_rotation(
        &mut left,
        &r,
        &enrolled,
        Some("work-2"),
        mover("work-2"),
        "pool-a",
        NOW,
    )
    .expect("an unproven account that cannot be run on is rotated past");
    assert_eq!(
        (left.previous.as_deref(), left.previous_proven),
        (Some("work-2"), false),
        "left before it ever answered"
    );
    strike(&mut left, "pool-a", "s-one", &Strike::SignedOut, NOW + 20);
    assert_eq!(
        (left.active.as_deref(), left.proven, left.previous_proven),
        (Some("work-2"), false, false)
    );
    // Its wall over, the session still at its wall on `main` is NOT told to
    // follow it: it moves there as the canary a rotation would have sent.
    let later = NOW + 2 * HOUR + RESET_GRACE_S + 1;
    assert_eq!(cond(&r, &left, "work-2", later), Condition::Low);
    assert_eq!(
        advise(
            &r,
            &left,
            &enrolled,
            &me("main"),
            &wall(),
            Mode::Early,
            later
        ),
        Advice::Move {
            to: "work-2".into(),
            ahead: false,
            rotates: false,
            takes_canary: true
        },
        "never a plain follow: it has not served a response"
    );
    // NEGATIVE CONTROL: the same history with the account PROVEN before it
    // was left comes back proven.
    let mut served = before.clone();
    prove(&mut served, "work-2", NOW + 5).expect("proven");
    note_wall(
        &mut served,
        "work-2",
        "five_hour",
        Some(NOW + 2 * HOUR),
        NOW + 6,
        NOW + 6,
    );
    begin_rotation(
        &mut served,
        &r,
        &enrolled,
        Some("work-2"),
        mover("work-2"),
        "pool-a",
        NOW + 6,
    )
    .expect("rotates");
    assert!(served.previous_proven);
    strike(&mut served, "pool-a", "s-one", &Strike::SignedOut, NOW + 20);
    assert_eq!(
        (served.active.as_deref(), served.proven),
        (Some("work-2"), true)
    );

    // NEGATIVE CONTROL: once it HAS answered, both go through.
    let mut answered = before.clone();
    prove(&mut answered, "work-2", NOW + 10).expect("proven");
    assert_eq!(
        ahead(&answered),
        Advice::Move {
            to: "pool-a".into(),
            ahead: true,
            rotates: true,
            takes_canary: false
        }
    );
    assert!(
        begin_rotation(
            &mut answered,
            &r,
            &enrolled,
            Some("work-2"),
            mover("work-2"),
            "pool-a",
            NOW + 10
        )
        .is_ok()
    );

    // An unproven account that can NOT be run on holds nobody: a wall
    // recorded on it (with no strike) is rotated past, by its own session too.
    let mut walled = before.clone();
    limited(&mut walled, "work-2", NOW + 2 * HOUR);
    assert_eq!(
        advise(
            &r,
            &walled,
            &enrolled,
            &me("work-2"),
            &wall(),
            Mode::Early,
            NOW
        ),
        Advice::Move {
            to: "pool-a".into(),
            ahead: false,
            rotates: true,
            takes_canary: false
        }
    );
    assert!(
        begin_rotation(
            &mut walled,
            &r,
            &enrolled,
            Some("work-2"),
            mover("work-2"),
            "pool-a",
            NOW
        )
        .is_ok()
    );
}

/// THE CANARY'S HOLD LASTS TEN MINUTES, THEN THE NEXT SESSION TAKES ITS
/// PLACE. A canary whose tab was closed, or that sits idle with nothing to
/// send, must not hold every other session for good.
///
/// `active` is never put back by the clock. The session on the unproven
/// account stays; the next one to reach a point moves onto it and is the
/// canary from then on.
#[test]
fn the_canarys_hold_lasts_ten_minutes_then_the_next_session_takes_its_place() {
    assert_eq!(CANARY_HOLD_S, 600);
    let r = three();
    let enrolled = all(&r);
    let mut s = on_main();
    limited(&mut s, "main", NOW + DAY);
    let won = begin_rotation(
        &mut s,
        &r,
        &enrolled,
        Some("main"),
        mover("main"),
        "work-2",
        NOW,
    )
    .expect("rotates");
    assert_eq!(
        won.sid.as_deref(),
        Some("s-one"),
        "the row names its session"
    );
    let first = Canary {
        sid: "s-one".into(),
        at: NOW,
        took: false,
    };
    assert_eq!(s.canary, Some(first.clone()));
    assert_eq!((s.canary_since(), s.canary_holds(NOW)), (Some(NOW), true));
    // Nothing more is heard of it: no `prove`, no `strike`.

    let at_wall =
        |s: &State, now: i64| advise(&r, s, &enrolled, &me("main"), &wall(), Mode::Early, now);
    let on_pool = |s: &State, now: i64| {
        advise(
            &r,
            s,
            &enrolled,
            &me("pool-a"),
            &Point::TurnEnd,
            Mode::Wall,
            now,
        )
    };
    let take = |ahead: bool| Advice::Move {
        to: "work-2".into(),
        ahead,
        rotates: false,
        takes_canary: true,
    };
    let rotate_past = |s: &State, now: i64| {
        begin_rotation(
            &mut s.clone(),
            &r,
            &enrolled,
            Some("work-2"),
            mover("main"),
            "pool-a",
            now,
        )
    };
    let held = Err(Lost::Held {
        active: "work-2".into(),
    });

    // While the hold stands everybody else stays, at a wall or away from
    // one, and the place is not to be taken…
    let last = NOW + CANARY_HOLD_S - 1;
    assert_eq!(at_wall(&s, last), Advice::Stay(StayWhy::Canary));
    assert_eq!(on_pool(&s, last), Advice::Stay(StayWhy::Canary));
    assert_eq!(rotate_past(&s, last), held);
    assert_eq!(
        take_canary(&mut s.clone(), Some(&first), "s-two", last),
        None
    );

    // …and not a second longer.
    let lapsed = NOW + CANARY_HOLD_S;
    assert!(!s.canary_holds(lapsed));
    assert_eq!(at_wall(&s, lapsed), take(false));
    assert_eq!(on_pool(&s, lapsed), take(true));
    assert_eq!(at_wall(&s, NOW + 30 * DAY), take(false), "nor a month on");
    // The session ON the unproven account stays where it is, and `active`
    // is not rotated past for the hold having lapsed.
    assert_eq!(
        advise(
            &r,
            &s,
            &enrolled,
            &me("work-2"),
            &Point::TurnEnd,
            Mode::Wall,
            lapsed
        ),
        Advice::Stay(StayWhy::NothingDue)
    );
    assert_eq!(rotate_past(&s, lapsed), held);

    // THE TAKE: a compare-and-set on the canary's record. Nothing else
    // moves — not `active`, not the proof, not the hour's rotations. (What
    // the rotation wrote over goes with the record: it is no longer the
    // silent canary's to take back.)
    let before = s.clone();
    let took = take_canary(&mut s, Some(&first), "s-two", lapsed).expect("takes the place");
    assert_eq!(
        (
            took.kind,
            took.account.as_str(),
            took.to.as_deref(),
            took.sid.as_deref(),
            took.why.as_str()
        ),
        (
            Kind::Rotate,
            "work-2",
            Some("work-2"),
            Some("s-two"),
            "canary: in place of s-one, which did not answer"
        )
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
    // A third session that read the same state loses — whatever its clock
    // says, because the compare is on the record — and is held by the NEW
    // canary, for ten minutes of its own.
    for clock in [lapsed, lapsed - 1, lapsed + DAY] {
        let mut raced = s.clone();
        assert_eq!(
            take_canary(&mut raced, Some(&first), "s-three", clock),
            None
        );
        assert_eq!(raced, s);
    }
    assert_eq!(at_wall(&s, lapsed), Advice::Stay(StayWhy::Canary));
    assert_eq!(
        at_wall(&s, lapsed + CANARY_HOLD_S - 1),
        Advice::Stay(StayWhy::Canary)
    );
    assert_eq!(at_wall(&s, lapsed + CANARY_HOLD_S), take(false));

    // THE CANARY'S AGENT ENDED: forgetting its session ends its hold at
    // once. Forgetting any other session does not.
    let mut gone = s.clone();
    assert!(!forget_session(&mut gone, "s-nobody"));
    assert_eq!(at_wall(&gone, lapsed + 1), Advice::Stay(StayWhy::Canary));
    assert!(forget_session(&mut gone, "s-two"));
    assert_eq!(gone.canary, None);
    assert_eq!(at_wall(&gone, lapsed + 1), take(false));
    let row = take_canary(&mut gone, None, "s-three", lapsed + 1).expect("nobody held it");
    assert_eq!(row.why, "canary: no session held it");

    // A hold stamped ahead of the clock (a clock set back) is no hold.
    let ahead = State {
        canary: Some(Canary {
            at: lapsed + DAY,
            ..second.clone()
        }),
        ..s.clone()
    };
    assert_eq!(at_wall(&ahead, lapsed), take(false));

    // SERVED, at last: the canary's work is done, and the others follow
    // plainly.
    let mut served = s.clone();
    prove(&mut served, "work-2", lapsed + 5).expect("proven");
    assert_eq!(served.canary, None);
    assert_eq!(
        at_wall(&served, lapsed + 5),
        Advice::Move {
            to: "work-2".into(),
            ahead: false,
            rotates: false,
            takes_canary: false
        }
    );
    assert_eq!(
        take_canary(&mut served, None, "s-three", lapsed + DAY),
        None,
        "a proven account has no place to take"
    );

    // The owner's word settles it as a served response does: pinned, the
    // account is proven, and no canary is left on record.
    let mut owned = s.clone();
    pin(&mut owned, &r, &enrolled, "work-2", lapsed + 5).expect("pins");
    assert_eq!((owned.proven, &owned.canary), (true, &None));

    // THE TAKER'S RELAUNCH IS REFUSED (its agent has jobs): it gives up its
    // hold, and undoes NO rotation — it made none. `active` stays, the
    // hour's count stays, and the next session takes the place.
    let not_moved = Strike::NotMoved("wait:changed".into());
    let mut refused = s.clone();
    let events = strike(&mut refused, "work-2", "s-two", &not_moved, lapsed + 2);
    assert_eq!(events.len(), 1);
    assert_eq!(
        (
            events[0].kind,
            events[0].to.as_deref(),
            events[0].why.as_str()
        ),
        (Kind::Struck, None, "not moved: wait:changed")
    );
    assert_eq!(
        refused,
        State {
            canary: None,
            ..s.clone()
        }
    );
    assert_eq!(refused.rotations, [NOW]);
    assert_eq!(at_wall(&refused, lapsed + 2), take(false));
    // NEGATIVE CONTROL: from the session that MADE the rotation the same
    // word undoes it and un-counts it.
    let mut undone = before.clone();
    strike(&mut undone, "work-2", "s-one", &not_moved, NOW + 2);
    assert_eq!(
        (
            undone.active.as_deref(),
            undone.proven,
            undone.rotations.len(),
            &undone.canary
        ),
        (Some("main"), true, 0, &None)
    );
    // And a taker the account answers with a wall is struck as any canary
    // is: `active` goes back.
    let mut walled = s.clone();
    let own_wall = Strike::Wall {
        window: "five_hour".into(),
        until: Some(lapsed + HOUR),
    };
    strike(&mut walled, "work-2", "s-two", &own_wall, lapsed + 2);
    assert_eq!(
        (walled.active.as_deref(), walled.proven, &walled.canary),
        (Some("main"), true, &None)
    );

    // AN UNPROVEN ACCOUNT THAT IS LOW is taken at a wall, where there is a
    // need, and not at a plain turn end: a session doing fine elsewhere is
    // not relaunched onto a nearly spent account to prove it.
    let mut low_one = s.clone();
    low(&mut low_one, "work-2");
    let later = lapsed + CANARY_HOLD_S;
    assert_eq!(at_wall(&low_one, later), take(false));
    assert_eq!(on_pool(&low_one, later), Advice::Stay(StayWhy::Canary));
}

/// A RELAUNCH THE HOST REFUSES IS THE WORD OF THE SESSION THAT WAS TO MOVE,
/// AND IT TAKES THE ROTATION BACK EXACTLY.
///
/// `strike` does not decide from the canary's record alone: the rotation's
/// maker, reporting `NotMoved` after another session took its place, must
/// not drop the TAKER's hold. And undoing a rotation past an account that
/// had itself not answered restores that account's `previous`, proof and
/// canary record, so a later strike of it does not go back to nothing.
#[test]
fn a_refused_relaunch_is_its_own_sessions_word_and_takes_the_rotation_back_exactly() {
    let r = three();
    let enrolled = all(&r);
    let not_moved = Strike::NotMoved("wait:changed".into());
    let rotate = |s: &mut State, sid, from, to, now| {
        let read = s.active.clone();
        let who = Mover { sid, account: from };
        begin_rotation(s, &r, &enrolled, read.as_deref(), who, to, now).expect("rotates")
    };
    let at_wall =
        |s: &State, now: i64| advise(&r, s, &enrolled, &me("main"), &wall(), Mode::Early, now);

    // `s-one` rotates main -> work-2 and is never heard of; past its ten
    // minutes `s-two` takes its place.
    let mut start = on_main();
    limited(&mut start, "main", NOW + DAY);
    let mut s = start.clone();
    rotate(&mut s, "s-one", "main", "work-2", NOW);
    let rotated = s.clone();
    assert_eq!(rotated.undo, [Undo::default()], "what it wrote over");
    let then = NOW + CANARY_HOLD_S + 100;
    take_canary(&mut s, rotated.canary.as_ref(), "s-two", then).expect("takes the place");
    let taken = s.clone();

    // ONLY NOW is the maker's deferred relaunch refused. It is not the
    // canary any more: no step and no row, and the taker's hold stands.
    assert!(strike(&mut s, "work-2", "s-one", &not_moved, then + 1).is_empty());
    assert_eq!(s, taken);
    assert_eq!(at_wall(&s, then + 1), Advice::Stay(StayWhy::Canary));
    // Nor is it the word of a session that is on no record, or of none.
    for sid in ["s-three", "", " "] {
        assert!(
            strike(&mut s, "work-2", sid, &not_moved, then + 1).is_empty(),
            "{sid:?}"
        );
        assert_eq!(s, taken, "{sid:?}");
    }
    // NEGATIVE CONTROL: the taker's own word is acted on. It gives up its
    // hold, and its row names it.
    let events = strike(&mut s, "work-2", "s-two", &not_moved, then + 1);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].sid.as_deref(), Some("s-two"));
    assert_eq!(
        s,
        State {
            canary: None,
            ..taken.clone()
        }
    );
    // …after which nobody's is: the record is gone, and the rotation stands.
    let given_up = s.clone();
    for sid in ["s-one", "s-two"] {
        assert!(strike(&mut s, "work-2", sid, &not_moved, then + 2).is_empty());
    }
    assert_eq!(s, given_up);
    // A WORD ABOUT THE ACCOUNT is anyone's on it: its wall, heard by the
    // session the taker took the place of, strikes it.
    let mut walled = taken.clone();
    let own_wall = Strike::Wall {
        window: "five_hour".into(),
        until: Some(then + HOUR),
    };
    let events = strike(&mut walled, "work-2", "s-one", &own_wall, then + 1);
    assert_eq!(events[0].sid.as_deref(), Some("s-one"));
    assert_eq!(
        (walled.active.as_deref(), walled.proven),
        (Some("main"), true)
    );

    // BEFORE THE TAKE the word is the maker's and nobody else's, and from
    // the maker the state is what it was before the rotation, whole.
    let mut early = rotated.clone();
    assert!(strike(&mut early, "work-2", "s-two", &not_moved, NOW + 5).is_empty());
    assert_eq!(early, rotated);
    assert_eq!(
        strike(&mut early, "work-2", "s-one", &not_moved, NOW + 5).len(),
        1
    );
    assert_eq!(early, start);

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
    assert_eq!(
        State::from_json(&s.to_json(), seen + 30),
        Ok(s.clone()),
        "it is on file"
    );
    let second = s.clone();
    let events = strike(&mut s, "pool-a", "s-two", &not_moved, seen + 35);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].to.as_deref(), Some("work-2"));
    assert_eq!(s, left, "everything the rotation wrote is put back");
    assert_eq!(
        (s.previous.as_deref(), s.previous_proven, &s.canary),
        (Some("main"), true, &rotated.canary)
    );
    assert!(s.canary_holds(seen + 35), "its hold was never given up");
    // So a strike of work-2 goes back to main, proven…
    let mut struck = s.clone();
    strike(
        &mut struck,
        "work-2",
        "s-one",
        &Strike::SignedOut,
        seen + 40,
    );
    assert_eq!(
        (struck.active.as_deref(), struck.proven, struck.undo.len()),
        (Some("main"), true, 0)
    );
    // …and the first rotation, refused in its turn, is taken back as
    // exactly: the state before either, with the wall that was seen since.
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
    // NEGATIVE CONTROL: a state written before the undo was kept has
    // nothing to put back but `active` and its proof.
    let mut older = State {
        undo: Vec::new(),
        ..second.clone()
    };
    strike(&mut older, "pool-a", "s-two", &not_moved, seen + 35);
    assert_eq!(
        older,
        State {
            previous: None,
            previous_proven: false,
            canary: None,
            undo: Vec::new(),
            ..left.clone()
        }
    );

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
    // has spoken, or the canary's agent has ended.
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
        assert_eq!(
            State::from_json(&s.to_json(), at),
            Ok(s.clone()),
            "it is on file: {word:?}"
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

    // NEGATIVE CONTROL: the relaunch REFUSED instead is no answer of the
    // account's: the same account gone back to, but the rotation is not
    // counted and nothing is held against pool-a.
    let mut refused = second.clone();
    let not_moved = Strike::NotMoved("wait:changed".into());
    strike(&mut refused, "pool-a", "s-two", &not_moved, seen + 40);
    assert_eq!(refused, left);
    assert_eq!(refused.rotations.len(), 1);
    assert_eq!(cond(&r, &refused, "pool-a", seen + 40), Condition::Unseen);

    // A STRIKE OF AN ACCOUNT CHOSEN FROM A PROVEN ONE restores what THAT
    // rotation found, which is nothing to go back from: `previous` and the
    // canary are none, as before.
    let mut first = start.clone();
    rotate(&mut first, "s-one", "main", "work-2", NOW);
    strike(&mut first, "work-2", "s-one", &words[0], NOW + 5);
    assert_eq!(
        (
            first.active.as_deref(),
            first.proven,
            first.previous.as_deref(),
            &first.canary,
            first.undo.len()
        ),
        (Some("main"), true, None, &None, 0)
    );
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

    // THE ORDER: a wall on the pinned account ENDS the pin, so the session
    // on it is advised the rotation — and the compare-and-set, which reads
    // the state and not the point, commits it once the wall is recorded.
    let rotate = Advice::Move {
        to: "work-2".into(),
        ahead: false,
        rotates: true,
        takes_canary: false,
    };
    let commit = |s: &State| {
        let who = mover("main");
        begin_rotation(
            &mut s.clone(),
            &r,
            &enrolled,
            Some("main"),
            who,
            "work-2",
            NOW,
        )
    };
    let mut pinned = on_main();
    pin(&mut pinned, &r, &enrolled, "main", NOW - MIN).expect("pins");
    // …and the canary's own account, which nobody rotates past while it
    // can be run on.
    let unanswered = State {
        active: Some("main".into()),
        proven: false,
        canary: Some(Canary {
            sid: "s-one".into(),
            at: NOW - MIN,
            took: false,
        }),
        ..state()
    };
    for (s, unrecorded) in [
        (&pinned, Lost::Pinned),
        (
            &unanswered,
            Lost::Held {
                active: "main".into(),
            },
        ),
    ] {
        assert_eq!(at_wall(s), rotate);
        let walled = recorded(s);
        assert_eq!(at_wall(&walled), rotate);
        assert!(!walled.pinned, "the wall ended the pin");
        assert!(commit(&walled).is_ok());
        // NEGATIVE CONTROL: on the state as it was, the wall not recorded,
        // the same rotation is refused. Recording it first is the contract.
        assert_eq!(commit(s), Err(unrecorded));
    }
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
    begin_rotation(
        &mut moved,
        &r,
        &enrolled,
        Some("main"),
        mover("main"),
        "work-2",
        NOW,
    )
    .expect("rotates");
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
    assert_eq!(ROTATION_WINDOW_S, 3600);
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
        begin_rotation(
            &mut s,
            &r,
            &enrolled,
            Some("main"),
            mover("main"),
            "work-2",
            NOW
        ),
        Err(Lost::Spent)
    );
    assert_eq!(s, before);
    // The early move is bounded by the same count.
    let mut s = with(times(bound));
    low(&mut s, "main");
    assert_eq!(
        advise(
            &r,
            &s,
            &enrolled,
            &me("main"),
            &Point::TurnEnd,
            Mode::Early,
            NOW
        ),
        Advice::Stay(StayWhy::Breaker { until: lapses })
    );
    // FOLLOWING a proven account is not a rotation and is not bounded here.
    let mut s = with(times(bound));
    s.active = Some("work-2".into());
    assert!(matches!(
        advise(&r, &s, &enrolled, &me("main"), &wall(), Mode::Early, NOW),
        Advice::Move { rotates: false, .. }
    ));

    // NEGATIVE CONTROL: one under the bound rotates, the times older than
    // the window are dropped as it does, and the next one is refused.
    let mut old = times(bound - 1);
    old.push(NOW - 2 * HOUR);
    let mut s = with(old);
    limited(&mut s, "main", NOW + HOUR);
    assert!(
        begin_rotation(
            &mut s,
            &r,
            &enrolled,
            Some("main"),
            mover("main"),
            "work-2",
            NOW
        )
        .is_ok()
    );
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
    assert_eq!(events[0].why, "session 40% week 10%");
    let second = Sample {
        at: NOW,
        session_pct: None,
        ..sample(0, 12)
    };
    assert_eq!(
        note_sample(&mut s, "main", second.clone(), NOW)[0].why,
        "session ?% week 12%"
    );
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
    assert!(note_offer(&mut s, "main", 2, Some(NOW + DAY), NOW, NOW));
    assert!(!note_offer(&mut s, "main", 9, None, NOW - 1, NOW), "older");
    assert_eq!(
        s.accounts["main"]
            .offer
            .as_ref()
            .map(|o| (o.left, o.by.as_str())),
        Some((2, "0a0b"))
    );
    assert!(note_offer(&mut s, "main", 1, None, NOW + 1, NOW + 1));
    assert_eq!(
        s.accounts["main"]
            .offer
            .as_ref()
            .map(|o| (o.left, o.use_by)),
        Some((1, None))
    );

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
    for (auth, word) in [
        (Auth::Unseen, "unseen"),
        (Auth::Ok, "ok"),
        (Auth::SignedOut, "signed-out"),
    ] {
        assert_eq!((auth.as_str(), Auth::parse(word)), (word, auth));
    }
    assert_eq!(Auth::parse("root"), Auth::Unseen, "never a stronger word");

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
    begin_rotation(
        &mut s,
        &r,
        &enrolled,
        Some("main"),
        mover("main"),
        "work-2",
        NOW,
    )
    .expect("rotates");
    assert_eq!(s.rotations, [NOW]);

    // THE PROBE'S CLAIM: one stamped tomorrow holds nobody off.
    let mut s = on_main();
    s.probe = Some(ProbeClaim {
        account: "main".into(),
        sid: "s-x".into(),
        at: ahead,
    });
    assert_eq!(
        advise(
            &r,
            &s,
            &enrolled,
            &me("main"),
            &Point::TurnEnd,
            Mode::Early,
            NOW
        ),
        Advice::Probe
    );
    assert!(claim_probe(&mut s, "main", "s-1", NOW).is_some());

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
            note_offer(held, "main", 0, None, NOW, NOW),
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
        assert!(!note_offer(held, "main", 5, None, NOW - 1, NOW));
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
            attempts: 0
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

#[test]
fn observations_merge_by_newest_and_machine_local_facts_never_do() {
    let wall = |until: i64, at: i64, by: &str| Wall {
        until: Some(until),
        at,
        by: by.into(),
        attempts: 0,
    };
    let at = |pct: u32, at: i64, by: &str| Sample {
        at,
        by: by.into(),
        ..sample(pct, pct)
    };
    let offer = |left: u32, at: i64, by: &str| Offer {
        left,
        use_by: None,
        at,
        by: by.into(),
    };

    let mut a = State {
        machine: "aa".into(),
        active: Some("main".into()),
        proven: true,
        rotations: vec![NOW],
        ..State::default()
    };
    a.sessions.insert(
        "s-a".into(),
        SessionRow {
            account: "main".into(),
            at: NOW,
            held: None,
        },
    );
    let main = a.accounts.entry("main".into()).or_default();
    main.walls
        .insert("five_hour".into(), wall(NOW + HOUR, 100, "aa"));
    main.walls
        .insert("seven_day".into(), wall(NOW + DAY, 500, "aa"));
    main.sample = Some(at(30, 300, "aa"));
    main.before = Some(at(10, 100, "aa"));
    main.offer = Some(offer(2, 100, "aa"));
    main.auth = Auth::Ok;
    main.auth_at = Some(50);

    let mut b = State {
        machine: "bb".into(),
        active: Some("pool-a".into()),
        previous: Some("main".into()),
        pinned: true,
        rotations: vec![NOW - 1, NOW - 2],
        probe: Some(ProbeClaim {
            account: "main".into(),
            sid: "s-b".into(),
            at: NOW,
        }),
        reset: Some(ResetClaim {
            account: "main".into(),
            until: None,
            sid: "s-b".into(),
            at: NOW,
            pid: 7,
            start: "1".into(),
            done: None,
        }),
        ..State::default()
    };
    b.sessions.insert(
        "s-b".into(),
        SessionRow {
            account: "pool-a".into(),
            at: NOW,
            held: None,
        },
    );
    let main = b.accounts.entry("main".into()).or_default();
    main.walls
        .insert("five_hour".into(), wall(NOW + 6 * HOUR, 200, "bb"));
    main.walls
        .insert("seven_day".into(), wall(NOW + DAY, 400, "bb"));
    main.sample = Some(at(25, 250, "bb"));
    main.before = Some(at(20, 200, "bb"));
    main.offer = Some(offer(1, 200, "bb"));
    main.auth = Auth::SignedOut;
    main.auth_at = Some(999);
    main.ran_at = Some(NOW - 5);
    let pool = b.accounts.entry("pool-a".into()).or_default();
    pool.sample = Some(at(5, 50, "bb"));
    pool.auth = Auth::Ok;

    // BOTH WAYS: the observations are the same whichever side merges.
    let mut ab = a.clone();
    merge_observations(&mut ab, &b, &three(), NOW);
    let mut ba = b.clone();
    merge_observations(&mut ba, &a, &three(), NOW);
    for (name, merged) in [("a ← b", &ab), ("b ← a", &ba)] {
        let main = &merged.accounts["main"];
        assert_eq!(
            main.walls["five_hour"],
            wall(NOW + 6 * HOUR, 200, "bb"),
            "{name}"
        );
        assert_eq!(
            main.walls["seven_day"],
            wall(NOW + DAY, 500, "aa"),
            "{name}"
        );
        assert_eq!(main.sample, Some(at(30, 300, "aa")), "{name}");
        assert_eq!(
            main.before,
            Some(at(25, 250, "bb")),
            "{name}: the next newest of the four"
        );
        assert_eq!(main.offer, Some(offer(1, 200, "bb")), "{name}");
        assert_eq!(
            merged.accounts["pool-a"].sample,
            Some(at(5, 50, "bb")),
            "{name}"
        );
    }

    // MACHINE-LOCAL facts are each side's own, untouched by the other's.
    assert_eq!(ab.machine, "aa");
    assert_eq!(
        (
            ab.active.as_deref(),
            ab.previous.as_deref(),
            ab.proven,
            ab.pinned
        ),
        (Some("main"), None, true, false)
    );
    assert_eq!(
        (ab.accounts["main"].auth, ab.accounts["main"].auth_at),
        (Auth::Ok, Some(50))
    );
    assert_eq!(
        ab.accounts["pool-a"].auth,
        Auth::Unseen,
        "another machine's login says nothing here"
    );
    assert_eq!(
        (ab.accounts["main"].ran_at, ba.accounts["main"].ran_at),
        (None, Some(NOW - 5)),
        "a session that ran on another machine did not run here"
    );
    assert_eq!(ab.sessions.keys().collect::<Vec<_>>(), ["s-a"]);
    assert_eq!(ab.rotations, [NOW]);
    assert_eq!((&ab.probe, &ab.reset), (&None, &None));
    assert_eq!(ba.machine, "bb");
    assert_eq!((ba.active.as_deref(), ba.pinned), (Some("pool-a"), true));
    assert_eq!(ba.accounts["main"].auth, Auth::SignedOut);
    assert_eq!(ba.sessions.keys().collect::<Vec<_>>(), ["s-b"]);
    assert_eq!(ba.rotations.len(), 2);
    assert!(ba.probe.is_some() && ba.reset.is_some());
    // NEGATIVE CONTROL: the merge did change something on each side.
    assert_ne!(ab, a);
    assert_ne!(ba, b);

    // Merging is idempotent, and one sample known to both is kept once.
    let again = ab.clone();
    merge_observations(&mut ab, &b, &three(), NOW);
    assert_eq!(ab, again);
    let mut twin = a.clone();
    merge_observations(&mut twin, &a.clone(), &three(), NOW);
    assert_eq!(twin, a);

    // A tie in time goes to the greater machine id, from either side.
    let tie = |mine: &str, theirs: &str| {
        let mut x = State::default();
        x.accounts.entry("main".into()).or_default().offer = Some(offer(1, 7, mine));
        let mut y = State::default();
        y.accounts.entry("main".into()).or_default().offer = Some(offer(2, 7, theirs));
        merge_observations(&mut x, &y, &three(), NOW);
        x.accounts["main"].offer.as_ref().map(|o| o.left)
    };
    assert_eq!((tie("aa", "bb"), tie("bb", "aa")), (Some(2), Some(1)));
}

/// THE MERGE KEEPS THE MODULE'S OWN RULES: it evicts none of this machine's
/// records for another machine's labels, makes room for a ninth wall by
/// the one that stops standing first, and brings back no wall the owner's
/// pin cleared (a pin on a walled account ends, with a ledger row).
#[test]
fn a_merge_evicts_nothing_of_ours_restores_no_cleared_wall_and_ends_a_pin_on_a_wall() {
    let r = three();
    let enrolled = all(&r);
    let theirs = || State {
        machine: "bb".into(),
        ..State::default()
    };
    let read = |by: &str| Sample {
        by: by.into(),
        ..sample(10, 20)
    };
    let wall_at = |until: i64, at: i64| Wall {
        until: Some(until),
        at,
        by: "bb".into(),
        attempts: 0,
    };

    // 1. ANOTHER MACHINE'S LABELS are not ours: none is taken, and none
    // costs this machine a record. A label the roster lists is taken.
    let mut mine = state();
    note_auth(&mut mine, "work-2", Auth::SignedOut, NOW - DAY);
    let mut crowd = theirs();
    for i in 0..MAX_ACCOUNTS {
        crowd.accounts.entry(format!("x{i:02}")).or_default().sample = Some(read("bb"));
    }
    crowd.accounts.entry("pool-a".into()).or_default().sample = Some(read("bb"));
    assert!(merge_observations(&mut mine, &crowd, &r, NOW).is_empty());
    assert_eq!(
        mine.accounts.keys().map(String::as_str).collect::<Vec<_>>(),
        ["pool-a", "work-2"]
    );
    assert_eq!(cond(&r, &mine, "work-2", NOW), Condition::SignedOut);
    // A full state: the listed label takes the place of the stalest record
    // the roster no longer lists — never of a listed one, however stale.
    let mut full = state();
    note_auth(&mut full, "work-2", Auth::SignedOut, NOW - DAY);
    for i in 0..(MAX_ACCOUNTS - 1) {
        let at = NOW + i64::try_from(i).expect("fits");
        note_auth(&mut full, &format!("old{i:02}"), Auth::Ok, at);
    }
    assert_eq!(full.accounts.len(), MAX_ACCOUNTS);
    assert!(merge_observations(&mut full, &crowd, &r, NOW).is_empty());
    assert_eq!(full.accounts.len(), MAX_ACCOUNTS);
    assert!(full.accounts.contains_key("pool-a") && !full.accounts.contains_key("old00"));
    assert_eq!(cond(&r, &full, "work-2", NOW), Condition::SignedOut);
    assert!(full.accounts.keys().all(|k| !k.starts_with('x')));

    // 2. A NINTH WALL takes the place of the one that stops standing first.
    let mut mine = state();
    for w in 0..MAX_WALLS {
        let until = NOW - DAY + i64::try_from(w).expect("fits");
        let window = format!("w{w}");
        note_wall(
            &mut mine,
            "work-2",
            &window,
            Some(until),
            NOW - 2 * DAY,
            NOW - 2 * DAY,
        );
    }
    assert_eq!(
        cond(&r, &mine, "work-2", NOW),
        Condition::Usable,
        "all lapsed"
    );
    let mut standing = theirs();
    standing
        .accounts
        .entry("work-2".into())
        .or_default()
        .walls
        .insert("five_hour".into(), wall_at(NOW + 2 * HOUR, NOW - MIN));
    assert!(merge_observations(&mut mine, &standing, &r, NOW).is_empty());
    assert_eq!(
        cond(&r, &mine, "work-2", NOW),
        Condition::Limited {
            until: NOW + 2 * HOUR + RESET_GRACE_S
        }
    );
    let walls = &mine.accounts["work-2"].walls;
    assert_eq!(walls.len(), MAX_WALLS);
    assert!(!walls.contains_key("w0"), "the most lapsed went");

    // 3. THE OWNER'S CLEARING STANDS: the pin clears the account's walls,
    // and the same walls coming back from another machine's copy of the
    // state — observed before the pin — are not taken.
    let mut mine = on_main();
    limited(&mut mine, "work-2", NOW + 2 * HOUR);
    let copy = State {
        machine: "bb".into(),
        ..mine.clone()
    };
    pin(&mut mine, &r, &enrolled, "work-2", NOW).expect("pins");
    assert_eq!(mine.accounts["work-2"].cleared_at, Some(NOW));
    assert!(merge_observations(&mut mine, &copy, &r, NOW + 5).is_empty());
    assert!(mine.accounts["work-2"].walls.is_empty());
    assert!(mine.pinned && cond(&r, &mine, "work-2", NOW + 5).fresh());

    // 4. A WALL ON THE PINNED ACCOUNT ENDS THE PIN, merged as noted: one
    // observed after the pin is taken, and the answer is the ledger's row.
    let mut after = theirs();
    after
        .accounts
        .entry("work-2".into())
        .or_default()
        .walls
        .insert("five_hour".into(), wall_at(NOW + 2 * HOUR, NOW + MIN));
    let mut lapsed = theirs();
    lapsed
        .accounts
        .entry("work-2".into())
        .or_default()
        .walls
        .insert("seven_day".into(), wall_at(NOW + 30, NOW + 20));
    let mut elsewhere = theirs();
    elsewhere
        .accounts
        .entry("pool-a".into())
        .or_default()
        .walls
        .insert("five_hour".into(), wall_at(NOW + 2 * HOUR, NOW + MIN));
    let then = NOW + 10 * MIN;
    // NEGATIVE CONTROLS: a wall on another account, and one that no longer
    // stands, leave the pin.
    assert!(merge_observations(&mut mine, &elsewhere, &r, then).is_empty());
    assert!(merge_observations(&mut mine, &lapsed, &r, then).is_empty());
    assert!(mine.pinned);
    let events = merge_observations(&mut mine, &after, &r, then);
    assert_eq!(
        events
            .iter()
            .map(|e| (e.kind, e.account.as_str(), e.why.as_str(), e.t))
            .collect::<Vec<_>>(),
        [(Kind::Unpin, "work-2", "wall", then)]
    );
    assert!(!mine.pinned);
    assert!(matches!(
        cond(&r, &mine, "work-2", then),
        Condition::Limited { .. }
    ));

    // The same for a wall NOTED again on a pinned account that already
    // holds it (a state an older build left): no second `wall` row, and the
    // pin ends all the same.
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

    // 5. NO STAMP FROM THE FUTURE: a wall another machine stamped a day
    // ahead is taken as made now plus the skew, not as newer than anything
    // this machine will see for a day.
    let mut mine = state();
    let mut fast = theirs();
    fast.accounts
        .entry("main".into())
        .or_default()
        .walls
        .insert("five_hour".into(), wall_at(NOW + 2 * DAY, NOW + DAY));
    fast.accounts.entry("main".into()).or_default().offer = Some(Offer {
        left: 1,
        use_by: None,
        at: NOW + DAY,
        by: "bb".into(),
    });
    merge_observations(&mut mine, &fast, &r, NOW);
    let main = &mine.accounts["main"];
    assert_eq!(main.walls["five_hour"].at, NOW + STAMP_SKEW_S);
    assert_eq!(main.offer.as_ref().map(|o| o.at), Some(NOW + STAMP_SKEW_S));
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
        claim_reset(s, "main", until, sid, pid, START, now)
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
    assert_eq!(take(&mut s, "s-one", 43, until, NOW + 5), held);
    assert_eq!(
        claim_reset(
            &mut s,
            "main",
            until,
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
        claim_reset(&mut s, "work-2", until, "s-two", 42, START, NOW + 5),
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
        let said = claim_reset(&mut s, "personal", until, sid, pid, START, now);
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
    let used = ResetDone::Used { left: Some(1) };
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
            claim_reset(&mut empty, account, until, sid, pid, start, NOW),
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

/// A RESET USED, OR A LIMIT FOUND LIFTED, ENDS THE ACCOUNT'S WALLS FOR
/// EVERYONE: the walls are cleared and the instant kept, so the roster
/// answers `Resume` to a session still standing at the old row, and a look
/// at that row does not put the wall back. The row is `reset-used` only for
/// `Used`; a limit that lifted by itself is `reset-kept: lifted`. Each
/// outcome is recorded once and wakes the waiting tabs.
///
/// NEGATIVE CONTROLS: before any outcome the session at that row is rotated;
/// a reset KEPT or FAILED changes no wall and clears nothing, and the same
/// session is still rotated; and a reason is one bounded word, never a path
/// or an address.
#[test]
fn a_reset_used_or_a_limit_lifted_ends_the_walls_and_kept_or_failed_changes_none() {
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
            &wall_at(written),
            Mode::Wall,
            NOW,
        )
    };
    let limited_now = |s: &State| matches!(cond(&r, s, "main", NOW), Condition::Limited { .. });

    // Two walls on record for main, a pin's worth of machine-local facts,
    // and the claim.
    let mut walled = on_main();
    for (window, end) in [("seven_day", until), ("five_hour", NOW + 2 * HOUR)] {
        assert_eq!(
            note_wall(&mut walled, "main", window, Some(end), written, written).len(),
            1
        );
    }
    assert_eq!(
        claim_reset(&mut walled, "main", until, "s-one", 41, START, NOW - MIN),
        ResetClaimed::Taken
    );
    // NEGATIVE CONTROL: with the reset only claimed, the wall stands and the
    // session at its row is rotated.
    assert!(limited_now(&walled));
    assert_eq!(ask(&walled), rotate);

    for (done, kind, why) in [
        (ResetDone::Used { left: Some(1) }, Kind::ResetUsed, "1 left"),
        (ResetDone::Used { left: Some(0) }, Kind::ResetUsed, "0 left"),
        (ResetDone::Used { left: None }, Kind::ResetUsed, ""),
        (ResetDone::Lifted, Kind::ResetKept, "lifted"),
    ] {
        let mut s = walled.clone();
        let rows = reset_outcome(&mut s, "main", until, "s-one", &done, NOW);
        assert_eq!(
            rows,
            [Event::new(NOW, kind, "main").with_sid("s-one").because(why)],
            "{done:?}"
        );
        assert!(rows[0].wakes(), "{done:?}");
        let main = &s.accounts["main"];
        assert!(main.walls.is_empty(), "{done:?}: every wall of the account");
        assert_eq!(main.cleared_at, Some(NOW), "{done:?}");
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

        // THE SESSION AT THE OLD ROW IS RESUMED where it stands, and a look
        // at that row records no wall.
        assert!(!limited_now(&s), "{done:?}");
        assert_eq!(ask(&s), Advice::Resume { cleared_at: NOW }, "{done:?}");
        let before = s.clone();
        for window in ["seven_day", "five_hour"] {
            assert!(
                note_wall(&mut s, "main", window, Some(until), written, NOW).is_empty(),
                "{done:?} {window}"
            );
        }
        assert_eq!(s, before, "{done:?}");
        // With no roster file the same holds for the one row a reset is
        // judged by.
        let own = Roster::own();
        assert_eq!(
            advise(
                &own,
                &s,
                &all(&own),
                &me("main"),
                &wall_at(written),
                Mode::Wall,
                NOW
            ),
            Advice::Resume { cleared_at: NOW },
            "{done:?}"
        );
        // A wall met SINCE is a wall: recorded, and rotated from.
        let mut again = s.clone();
        assert_eq!(
            note_wall(&mut again, "main", "seven_day", Some(until), NOW, NOW).len(),
            1,
            "{done:?}"
        );
        assert_eq!(ask(&again), rotate, "{done:?}");
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

    // KEPT AND FAILED CHANGE NO WALL. The reason is the caller's word; a
    // vendor line's head and no more.
    for (done, kind, why) in [
        (ResetDone::Kept("early"), Kind::ResetKept, "early"),
        (
            ResetDone::Kept("check-timeout"),
            Kind::ResetKept,
            "check-timeout",
        ),
        (ResetDone::Failed("login"), Kind::ResetFailed, "login"),
        (ResetDone::Failed("unread"), Kind::ResetFailed, "unread"),
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
    assert_eq!(
        [
            ResetDone::Used { left: None }.word(),
            ResetDone::Lifted.word(),
            ResetDone::Kept("").word(),
            ResetDone::Failed("").word()
        ],
        ["used", "lifted", "kept", "failed"]
    );

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

    // THROUGH THE STORE: the claim alone wakes nobody (it writes no row);
    // an outcome, kept included, replaces the wake marker.
    let dir = tmp("reset-wake");
    let store = Store::at(dir.path());
    let mut note = None;
    let rows = store
        .transact(NOW, &mut note, |state| {
            let taken = claim_reset(state, "main", until, "s-one", 41, START, NOW);
            assert_eq!(taken, ResetClaimed::Taken);
            Ok(Vec::new())
        })
        .expect("claims");
    assert!(rows.is_empty() && !store.wake_path().exists());
    assert!(store.tail(8).expect("reads").is_empty(), "no ledger row");
    let rows = store
        .transact(NOW + 5, &mut note, |state| {
            Ok(reset_outcome(
                state,
                "main",
                until,
                "s-one",
                &ResetDone::Kept("early"),
                NOW + 5,
            ))
        })
        .expect("records");
    assert_eq!(rows.len(), 1);
    assert!(store.wake_path().exists(), "kept wakes the tabs it held");
    assert_eq!(
        store.tail(8).expect("reads"),
        [Event::new(NOW + 5, Kind::ResetKept, "main")
            .with_sid("s-one")
            .because("early")]
    );
    let (kept, _) = store.load(NOW + 6).expect("loads");
    assert_eq!(
        kept.reset
            .as_ref()
            .map(|c| (c.pid, c.start.as_str(), c.done.as_deref())),
        Some((41, START, Some("kept")))
    );
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
        let note = note.unwrap_or_else(|| panic!("{needle}: a note"));
        assert!(
            note.starts_with("state.json: ") && note.contains(needle),
            "{note}"
        );
        assert!(note.ends_with("read as an empty state"), "{note}");
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
        let why = store
            .load(NOW)
            .expect_err(&format!("{needle}: not ours to replace"));
        assert!(
            why.starts_with("state.json: ") && why.contains(needle),
            "{why}"
        );
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
    assert_eq!((LEDGER_MAX_BYTES, LEDGER_KEEP_ROWS), (1024 * 1024, 1024));
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
    store.touch_wake().expect("touches");
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
    assert_eq!(LOGIN_ENV, "CLAUDE_SECURESTORAGE_CONFIG_DIR");
    assert_eq!(HOME_ENV, "CLAUDE_CONFIG_DIR");

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

#[test]
fn the_hosts_route_and_the_roster_meet_the_process_login_in_one_place() {
    // THE HOST'S HALF (the settings files' route): not the default makes it
    // no stored login; the default leaves what the environment said, and
    // never makes a stored login of a process with a key.
    assert_eq!(stored().with_route_default(true), stored());
    assert!(!stored().with_route_default(false).stored_login);
    let keyed = env_of(&[("ANTHROPIC_API_KEY", "k")]);
    assert!(!keyed.clone().with_route_default(true).stored_login);
    assert!(!keyed.with_route_default(false).stored_login);
    let elsewhere = env_of(&[(HOME_ENV, "/elsewhere"), (LOGIN_ENV, "/logins/work-2")]);
    assert_eq!(
        elsewhere.clone().with_route_default(false),
        ProcessLogin {
            stored_login: false,
            ..elsewhere
        },
        "nothing else is touched"
    );

    // THE SESSION AS `advise` ASKS ABOUT IT: the row its login names, byte
    // for byte, and the two facts carried over.
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
    // …which is rule 1's stay, by its reason.
    let enrolled = all(&r);
    let stay = |me: &Me<'_>| advise(&r, &on_main(), &enrolled, me, &wall(), Mode::Early, NOW);
    assert_eq!(stay(&other), Advice::Stay(StayWhy::OtherHome));
    assert_eq!(
        stay(&env_of(&[("CLAUDE_CODE_USE_VERTEX", "1")]).me(&r)),
        Advice::Stay(StayWhy::NotAStoredLogin)
    );
    assert_eq!(
        stay(&env_of(&[(LOGIN_ENV, "/logins/nobody")]).me(&r)),
        Advice::Stay(StayWhy::NotListed)
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
    assert_eq!(judged_label(&r, &stored().with_route_default(false)), None);
    assert_eq!(judged_label(&r, &stored().with_profile_store(true)), None);
    assert_eq!(
        judged_label(&r, &stored().with_route_default(true)),
        Some("main")
    );
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
    // The host's roster is that rule over the window's own config path.
    assert_eq!(
        default_roster_path(),
        aterm_types::dirs::aterm_config_path().and_then(|config| roster_beside(&config))
    );
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
    assert_eq!(recorded.as_str(), "wall");

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
    assert_eq!(rows[0].why, format!("seven_day until {}", NOW + HOUR));
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
        assert_eq!(again.as_str(), "unchanged");
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
    assert_eq!(Recorded::NotListed.as_str(), "not-listed");
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
        stored().with_route_default(false),
    ] {
        assert_eq!(record(&r, &who), Recorded::NotStored, "{who:?}");
    }
    assert_eq!(Recorded::NotStored.as_str(), "not-stored");
    // A roster with no row for the login is said before the home is: there
    // is no account to speak of.
    let unlisted_elsewhere = env_of(&[(LOGIN_ENV, "/logins/nobody"), (HOME_ENV, "/elsewhere")]);
    assert_eq!(record(&r, &unlisted_elsewhere), Recorded::NotListed);

    // NOTHING WAS READ, MADE OR LOCKED for any of them: with no roster the
    // feature leaves no trace on the machine.
    assert!(!store.dir.exists(), "no directory, no lock, no state");

    // NEGATIVE CONTROL: the same call for a governed session records.
    assert_eq!(record(&r, &stored()).as_str(), "wall");
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
        assert_eq!(recorded.as_str(), "not-this-process");
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
        assert_eq!(recorded.as_str(), "wall", "{sid}: {recorded:?}");
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
    let enrolled = all(&r);
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
    // NEGATIVE CONTROL: the same row from a process on this machine's own
    // login is main's.
    let recorded = record_wall(&store, &r, "s-one", &stored(), &weekly(), STARTED, NOW);
    assert_eq!(recorded.as_str(), "wall", "{recorded:?}");
    let (after, _) = store.load(NOW).expect("ours");
    assert_eq!(cond(&r, &after, "main", NOW), stands);

    // IT DOES NOT STRIKE. `s-one` rotated to work-2, which has not answered,
    // and meets work-2's wall: the wall is on record and the rotation stands.
    let dir = tmp("recorder-no-strike");
    let store = Store::at(dir.path());
    let mut rotated = on_main();
    limited(&mut rotated, "main", NOW + DAY);
    begin_rotation(
        &mut rotated,
        &r,
        &enrolled,
        Some("main"),
        mover("main"),
        "work-2",
        NOW,
    )
    .expect("rotates");
    store.save(&rotated).expect("saves");
    let recorded = record_wall(&store, &r, "s-one", &work, &weekly(), STARTED, NOW);
    assert_eq!(recorded.as_str(), "wall", "{recorded:?}");
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
    let (recorded, state, rows) = record("recorder-no-reset", "s-one", &no_reset, NOW);
    assert_eq!(recorded.as_str(), "wall");
    assert_eq!(
        cond(&r, &state, "main", NOW),
        Condition::Limited {
            until: NOW - 10 + NO_RESET_WALL_S[0]
        }
    );
    assert_eq!(rows[0].why, "seven_day, no reset named");
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
        assert_eq!(recorded.as_str(), "wall", "{ahead}");
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
    assert_eq!(recorded.as_str(), "wall");
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
            .map(|e| (e.kind, e.sid.as_deref(), e.why.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                Kind::Wall,
                Some("s-one"),
                format!("seven_day until {}", NOW + HOUR).as_str()
            ),
            (Kind::Unpin, Some("s-one"), "wall"),
        ]
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
    assert_eq!(recorded.as_str(), "wall");
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
    assert_eq!(recorded.as_str(), "wall");
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
        assert_eq!(again.as_str(), "cleared");
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
    for (tag, not_ours) in [
        (
            "schema",
            r#"{"schema":2,"accounts":{"main":{"auth":"signed-out"}},"pinned":true}"#,
        ),
        ("no-schema", r#"{"accounts":{}}"#),
        ("array", "[1,2,3]"),
    ] {
        let dir = tmp(&format!("recorder-refused-{tag}"));
        let store = Store::at(dir.path());
        store.ensure().expect("dir");
        std::fs::write(store.state_file(), not_ours).expect("writes");
        let recorded = record_wall(&store, &r, "s-one", &stored(), &weekly(), STARTED, NOW);
        let Recorded::Refused(why) = &recorded else {
            panic!("{tag}: {recorded:?}");
        };
        assert_eq!(recorded.as_str(), "refused");
        assert!(
            why.contains("state.json") && why.contains("left as it is"),
            "{why}"
        );
        assert_eq!(
            std::fs::read_to_string(store.state_file()).expect("reads"),
            not_ours,
            "{tag}: byte for byte"
        );
        assert!(
            !store.ledger_path().exists(),
            "{tag}: no row of a wall not recorded"
        );
        assert!(!store.wake_path().exists(), "{tag}");
        // …and the same for a session that ended.
        let gone = record_gone(&store, "s-one", NOW);
        assert!(gone.is_err(), "{tag}: {gone:?}");
        assert_eq!(
            std::fs::read_to_string(store.state_file()).expect("reads"),
            not_ours,
            "{tag}"
        );
    }

    // A file too large to be ours is refused the same way.
    let dir = tmp("recorder-refused-large");
    let store = Store::at(dir.path());
    store.ensure().expect("dir");
    let large = " ".repeat(usize::try_from(MAX_STATE_BYTES).expect("fits") + 1);
    std::fs::write(store.state_file(), &large).expect("writes");
    let recorded = record_wall(&store, &r, "s-one", &stored(), &weekly(), STARTED, NOW);
    assert_eq!(recorded.as_str(), "refused", "{recorded:?}");
    assert_eq!(
        std::fs::metadata(store.state_file()).expect("stat").len(),
        large.len() as u64
    );

    // NEGATIVE CONTROL: a TORN file holds nothing to lose, and is replaced
    // by the state with the wall.
    let dir = tmp("recorder-torn");
    let store = Store::at(dir.path());
    store.ensure().expect("dir");
    std::fs::write(store.state_file(), r#"{"schema":1,"accou"#).expect("writes");
    let recorded = record_wall(&store, &r, "s-one", &stored(), &weekly(), STARTED, NOW);
    assert_eq!(recorded.as_str(), "wall", "{recorded:?}");
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
    assert_eq!(recorded.as_str(), "wall", "{recorded:?}");
}

#[test]
fn a_session_that_ended_is_forgotten_and_a_canarys_end_wakes_the_others() {
    let r = three();
    let enrolled = all(&r);

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
        assert_eq!(recorded.as_str(), "wall");
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
    begin_rotation(
        &mut s,
        &r,
        &enrolled,
        Some("main"),
        mover("main"),
        "work-2",
        NOW,
    )
    .expect("rotates");
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
        "{one}\n[[account]]\nlabel = \"work-2\"\nclass = \"dedicated\"\ndir = \"/logins/work-2\"\n"
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
    assert_eq!(RESET_BELIEVED_S, 8 * DAY);
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
    assert_eq!(
        store.tail(10).expect("reads")[0].why,
        "seven_day, no reset named"
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
    let rows = note_wall(&mut s, "main", "seven_day", Some(NOW + 9 * DAY), NOW, NOW);
    assert_eq!(rows[0].why, "seven_day, no reset named");
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
    assert_eq!(SESSION_KEEP_S, 30 * DAY);
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
    assert_eq!(
        PROFILE_KEYS,
        [
            "ANTHROPIC_PROFILE",
            "ANTHROPIC_FEDERATION_RULE_ID",
            "ANTHROPIC_ORGANIZATION_ID",
            "ANTHROPIC_IDENTITY_TOKEN",
            "ANTHROPIC_IDENTITY_TOKEN_FILE",
            "ANTHROPIC_CONFIG_DIR",
        ]
    );
    for name in PROFILE_KEYS {
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

    // WHAT FOLLOWS: rule 1's stay, and a wall recorded for nobody.
    let r = three();
    let helped = stored().with_settings(&named);
    assert_eq!(
        advise(
            &r,
            &on_main(),
            &all(&r),
            &helped.me(&r),
            &wall(),
            Mode::Early,
            NOW
        ),
        Advice::Stay(StayWhy::NotAStoredLogin)
    );
    let dir = tmp("recorder-settings");
    let store = Store::at(dir.path());
    assert_eq!(
        record_wall(&store, &r, "s-one", &helped, &weekly(), STARTED, NOW),
        Recorded::NotStored
    );
    assert!(!store.dir.exists(), "nothing was made for it");
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
    assert_eq!(
        LOCK_LADDER.iter().sum::<Duration>(),
        Duration::from_secs(2),
        "two seconds in all"
    );
    let r = three();
    let dir = tmp("lock-bounded");
    let store = Store::at(dir.path());
    // A state on record, so a session's end has something to lock for.
    let first = record_wall(&store, &r, "s-one", &stored(), &weekly(), STARTED, NOW);
    assert_eq!(first.as_str(), "wall");
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
    assert_eq!(walled.as_str(), "refused");
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
    assert_eq!(walled.as_str(), "wall");
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
            warned(NOW + MIN, false, 91, Some(session_end)),
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
                let taken = claim_reset(state, OWN, until, "s-1", 41, START, NOW + 30);
                assert_eq!(taken, ResetClaimed::Taken);
                Ok(reset_outcome(state, OWN, until, "s-1", done, end))
            })
            .expect("recorded");
        let (state, _) = store.load(end + 10).expect("ours");
        let lines = windows_full(&r, &state, end + 10);
        (dir, store, lines, state.account(OWN))
    };
    for done in [ResetDone::Used { left: Some(1) }, ResetDone::Lifted] {
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
                claim_reset(state, OWN, week_end, "s-a", 41, START, NOW),
                ResetClaimed::Taken
            );
            rows.extend(reset_outcome(
                state,
                OWN,
                week_end,
                "s-a",
                &ResetDone::Used { left: None },
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
            claim_reset(state, OWN, week_end, "s-1", 41, START, NOW + 10 * MIN),
            ResetClaimed::Taken
        );
        rows.extend(reset_outcome(
            state,
            OWN,
            week_end,
            "s-1",
            &ResetDone::Used { left: None },
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
