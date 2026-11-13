// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::ai::codex_limits::{CONTEXT_REACH, rollout_readings};

fn unix(s: &str) -> i64 {
    aterm_types::rfc3339::parse_utc_fractional(s).expect("a stamp")
}

use super::fixtures::{context, copied, count, limits_only, message, meta, meta_as, rate};

const LOGIN_A: (&str, &str) = ("acct-lab-1", "user-lab");

const ROOT: &str = "01900000-0000-7000-8000-000000000001";

const SUB: &str = "01900000-0000-7000-8000-000000000002";

const OTHER: &str = "01900000-0000-7000-8000-000000000003";

const T0: i64 = 1_790_000_000_000;

fn rows(reader: &mut TurnReader, text: &str) -> Vec<Row> {
    text.lines().map(|l| reader.line(l)).collect()
}

fn turns(rows: &[Row]) -> Vec<&Turn> {
    rows.iter()
        .filter_map(|r| match r {
            Row::Turn(t) => Some(t),
            _ => None,
        })
        .collect()
}

#[test]
fn each_served_turn_counts_once_on_its_turns_model() {
    let untimed = count(T0 + 20_000, (5000, 4000, 300), (2000, 1400, 150))
        .replace(&fixtures::stamp(T0 + 20_000), "not a stamp");
    let cut = count(T0 + 15_000, (4000, 3000, 200), (1000, 400, 50));
    let text = [
        meta(T0, ROOT, ROOT, None),
        context(T0 + 1000, "gpt-5.6-sol", None),
        count(T0 + 5000, (1000, 800, 50), (1000, 800, 50)),
        count(T0 + 5001, (1000, 800, 50), (1000, 800, 50)),
        limits_only(T0 + 5002),
        message(T0 + 5003),
        context(T0 + 9000, "gpt-6-luna", None),
        count(T0 + 12_000, (3000, 2600, 150), (2000, 1800, 100)),
        format!("{}\n", &cut[..cut.len() / 2]),
        untimed,
    ]
    .concat();
    let mut reader = TurnReader::default();
    let got = rows(&mut reader, &text);
    assert_eq!(got[0], Row::Other, "a session that is no fork");
    assert_eq!(reader.prefix, Prefix::Own);
    assert_eq!(got[3], Row::Repeat);
    assert_eq!(got[4], Row::Other, "the rate limits alone");
    assert_eq!(got[8], Row::Unparsed);
    assert_eq!(got[9], Row::Untimed);
    let served = turns(&got);
    assert_eq!(served.len(), 2);
    assert_eq!(served[0].at, (T0 + 5000) / 1000);
    assert_eq!(served[0].model.as_deref(), Some("gpt-5.6-sol"));
    assert_eq!(
        served[0].tokens,
        TurnTokens {
            input: 1000,
            cached: 800,
            cache_write: 0,
            output: 50,
            reasoning: 25,
        }
    );
    assert_eq!(served[1].model.as_deref(), Some("gpt-6-luna"));
    assert_eq!(served[1].tokens.input, 2000);
    assert_ne!(served[0].key, served[1].key);
    assert_eq!(reader.top, 5300, "the untimed turn still raised the total");
    let per_line = text
        .lines()
        .filter(|l| l.ends_with('}') && l.contains("\"last_token_usage\""))
        .count();
    assert_eq!(per_line, 4);
}

#[test]
fn a_forks_copied_prefix_counts_nothing() {
    let parent = [
        meta(T0, ROOT, ROOT, None),
        context(T0 + 1000, "gpt-5.6-sol", None),
        count(T0 + 5000, (1000, 800, 50), (1000, 800, 50)),
        count(T0 + 9000, (3000, 2600, 150), (2000, 1800, 100)),
    ]
    .concat();
    let fork_at = T0 + 60_000;
    let fork = [
        meta(fork_at, SUB, ROOT, Some(ROOT)),
        copied(&parent, fork_at + 1),
        message(fork_at + 2500),
        context(fork_at + 2501, "gpt-6-luna", None),
        count(fork_at + 7000, (3500, 2600, 250), (500, 0, 100)),
    ]
    .concat();
    let mut reader = TurnReader::default();
    let got = rows(&mut reader, &fork);
    assert_eq!(got.iter().filter(|r| **r == Row::Copied).count(), 2);
    let served = turns(&got);
    assert_eq!(served.len(), 1, "{got:?}");
    assert_eq!(served[0].model.as_deref(), Some("gpt-6-luna"));
    assert_eq!(served[0].tokens.input, 500);
    let parents = rows(&mut TurnReader::default(), &parent);
    assert_eq!(turns(&parents).len(), 2);
    let fork_keys: Vec<u64> = rows(
        &mut TurnReader::default(),
        &fork.replace(&format!(",\"forked_from_id\":\"{ROOT}\""), ""),
    )
    .iter()
    .filter_map(|r| match r {
        Row::Turn(t) => Some(t.key),
        _ => None,
    })
    .collect();
    assert_eq!(fork_keys.len(), 3, "the parent's two again, and its own");
    let parent_keys: Vec<u64> = turns(&parents).iter().map(|t| t.key).collect();
    assert_eq!(fork_keys[..2], parent_keys[..]);
}

#[test]
fn the_copy_ends_at_the_first_gap_past_the_burst() {
    let fork = |gap: i64| {
        [
            meta(T0, SUB, ROOT, Some(ROOT)),
            count(T0 + 400, (100, 0, 10), (100, 0, 10)),
            count(T0 + 800, (200, 0, 20), (100, 0, 10)),
            count(T0 + 800 + gap, (300, 0, 30), (100, 0, 10)),
        ]
        .concat()
    };
    let edge = rows(&mut TurnReader::default(), &fork(FORK_BURST_MS));
    assert!(turns(&edge).is_empty(), "{edge:?}");
    let past = rows(&mut TurnReader::default(), &fork(FORK_BURST_MS + 1));
    assert_eq!(turns(&past).len(), 1, "{past:?}");
    assert_eq!(turns(&past)[0].tokens.input, 100);
    let own = fork(FORK_BURST_MS).replace(&format!(",\"forked_from_id\":\"{ROOT}\""), "");
    assert_eq!(turns(&rows(&mut TurnReader::default(), &own)).len(), 3);
}

#[test]
fn a_count_repeating_the_copys_total_is_no_turn() {
    let parent = [
        meta(T0, ROOT, ROOT, None),
        context(T0 + 1000, "gpt-5.6-sol", None),
        count(T0 + 5000, (1000, 800, 50), (1000, 800, 50)),
        count(T0 + 9000, (3000, 2600, 150), (2000, 1800, 100)),
    ]
    .concat();
    let fork_at = T0 + 60_000;
    let own = [
        context(fork_at + 2000, "gpt-6-luna", None),
        count(fork_at + 2500, (3000, 2600, 150), (2000, 1800, 100)),
        count(fork_at + 7000, (3500, 2600, 250), (500, 0, 100)),
    ]
    .concat();
    let fork = [
        meta(fork_at, SUB, ROOT, Some(ROOT)),
        copied(&parent, fork_at + 1),
        own.clone(),
    ]
    .concat();
    let mut reader = TurnReader::default();
    let got = rows(&mut reader, &fork);
    assert_eq!(got.iter().filter(|r| **r == Row::Copied).count(), 2);
    assert_eq!(got.iter().filter(|r| **r == Row::Repeat).count(), 1);
    let served = turns(&got);
    assert_eq!(served.len(), 1, "{got:?}");
    assert_eq!(served[0].tokens.input, 500);
    assert_eq!(reader.top, 3750);
    let mut stopped = TurnReader::default();
    let lines: Vec<&str> = fork.lines().collect();
    for line in &lines[..lines.len() - 3] {
        stopped.line(line);
    }
    assert!(matches!(stopped.prefix, Prefix::Copy(_)));
    assert_eq!(stopped.top, 3150);
    let alone = [meta(fork_at, SUB, SUB, None), own].concat();
    assert_eq!(turns(&rows(&mut TurnReader::default(), &alone)).len(), 2);
}

#[test]
fn a_rollout_whose_first_line_names_no_session_is_read_as_a_fork() {
    let body = [
        count(T0, (100, 0, 10), (100, 0, 10)),
        count(T0 + 30, (200, 0, 20), (100, 0, 10)),
        context(T0 + 3000, "gpt-6-sol", None),
        count(T0 + 9000, (300, 0, 30), (100, 0, 10)),
    ]
    .concat();
    let got = rows(&mut TurnReader::default(), &format!("{{\"cut\n{body}"));
    assert_eq!(got[0], Row::Unparsed);
    assert_eq!(got[1], Row::Copied);
    assert_eq!(got[2], Row::Copied);
    assert_eq!(turns(&got).len(), 1);
    assert_eq!(turns(&got)[0].model.as_deref(), Some("gpt-6-sol"));
    let plain = format!("{}{body}", meta(T0 - 10, ROOT, ROOT, None));
    assert_eq!(turns(&rows(&mut TurnReader::default(), &plain)).len(), 3);
}

#[test]
fn a_service_tier_and_a_bad_model_are_read_as_they_are() {
    let text = |tier: Option<&str>, model: &str| {
        [
            meta(T0, ROOT, ROOT, None),
            context(T0 + 1, model, tier),
            count(T0 + 2, (10, 0, 1), (10, 0, 1)),
        ]
        .concat()
    };
    let turn = |tier, model| {
        let got = rows(&mut TurnReader::default(), &text(tier, model));
        turns(&got)[0].clone()
    };
    assert!(turn(Some("fast"), "gpt-6-sol").tier);
    assert!(turn(Some("flex"), "gpt-6-sol").tier);
    assert!(!turn(Some("standard"), "gpt-6-sol").tier);
    assert!(!turn(None, "gpt-6-sol").tier);
    assert_eq!(turn(None, "gpt 6 sol!").model, None);
    assert_eq!(turn(None, "gpt-6-sol").model.as_deref(), Some("gpt-6-sol"));
}

#[test]
fn a_whole_read_takes_complete_lines_within_its_cap() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("codex-usage-read-")
        .tempdir()
        .expect("temp dir");
    let path = dir.path().join("rollout.jsonl");
    let whole = [
        meta(T0, ROOT, ROOT, None),
        context(T0 + 1, "gpt-6-sol", None),
        count(T0 + 2, (10, 0, 1), (10, 0, 1)),
    ]
    .concat();
    let torn = count(T0 + 3, (20, 0, 2), (10, 0, 1));
    std::fs::write(&path, format!("{whole}{}", &torn[..torn.len() / 2])).expect("write");
    let mut n = 0;
    let read = read_rollout(&path, &mut TurnReader::default(), u64::MAX, |r| {
        n += usize::from(matches!(r, Row::Turn(_)));
    })
    .expect("read");
    assert_eq!(n, 1, "the torn count is not read");
    assert!(read.complete);
    assert_eq!(read.read, std::fs::metadata(&path).expect("meta").len());
    let short = read_rollout(&path, &mut TurnReader::default(), 10, |_| {}).expect("read");
    assert!(!short.complete);
    assert_eq!(short.read, 10);
    let len = std::fs::metadata(&path).expect("meta").len();
    assert!(
        read_rollout(&path, &mut TurnReader::default(), len, |_| {})
            .expect("read")
            .complete
    );
    std::fs::write(&path, &whole).expect("write");
    let full = read_rollout(&path, &mut TurnReader::default(), u64::MAX, |_| {}).expect("read");
    assert!(full.complete);
    assert!(
        read_rollout(
            &dir.path().join("none"),
            &mut TurnReader::default(),
            1,
            |_| {}
        )
        .is_none()
    );
}

#[test]
fn a_conversation_is_its_tuis_thread_and_the_subagents_it_spawned() {
    let home = aterm_tempfile::Builder::new()
        .prefix("codex-usage-conv-")
        .tempdir()
        .expect("temp dir");
    let day = home.path().join("sessions/2026/10/15");
    std::fs::create_dir_all(&day).expect("mkdir");
    let write = |name: &str, body: String, age_s: u64| {
        let path = day.join(name);
        std::fs::write(&path, body).expect("write");
        std::fs::File::options()
            .write(true)
            .open(&path)
            .and_then(|f| f.set_modified(SystemTime::now() - std::time::Duration::from_secs(age_s)))
            .expect("mtime");
        path
    };
    let root = write(
        &format!("rollout-2026-10-15T10-00-00-{ROOT}.jsonl"),
        meta(T0, ROOT, ROOT, None),
        30,
    );
    let sub = write(
        &format!("rollout-2026-10-15T10-05-00-{SUB}.jsonl"),
        meta(T0, SUB, ROOT, Some(ROOT)),
        20,
    );
    let other = write(
        &format!("rollout-2026-10-15T11-00-00-{OTHER}.jsonl"),
        meta(T0, OTHER, OTHER, None),
        10,
    );
    write(
        "rollout-2026-10-15T12-00-00-cut.jsonl",
        "{\"cut".to_owned(),
        5,
    );
    for named in [SUB, ROOT] {
        let c = conversation_rollouts(home.path(), named);
        assert_eq!(c.root, ROOT, "named {named}");
        assert_eq!(c.files, [root.clone(), sub.clone()], "named {named}");
        assert_eq!(c.subagents, 1);
        assert_eq!(c.unread, 1, "the cut first line");
    }
    assert_eq!(
        conversation_rollouts(home.path(), SUB).own.as_deref(),
        Some(sub.as_path())
    );
    let c = conversation_rollouts(home.path(), OTHER);
    assert_eq!(c.root, OTHER);
    assert_eq!(c.files, [other]);
    assert_eq!(c.subagents, 0);
    let none = conversation_rollouts(home.path(), "01900000-0000-7000-8000-00000000000f");
    assert!(none.own.is_none() && none.files.is_empty());
}

#[test]
fn a_stamp_reads_to_the_millisecond() {
    let s = unix("2026-08-01T17:36:47Z");
    assert_eq!(stamp_ms("2026-08-01T17:36:47.998Z"), Some(s * 1000 + 998));
    assert_eq!(stamp_ms("2026-08-01T17:36:47.9Z"), Some(s * 1000 + 900));
    assert_eq!(
        stamp_ms("2026-08-01T17:36:47.123456Z"),
        Some(s * 1000 + 123)
    );
    assert_eq!(stamp_ms("2026-08-01T17:36:47Z"), Some(s * 1000));
    assert_eq!(fixtures::stamp(s * 1000 + 7), "2026-08-01T17:36:47.007Z");
    assert_eq!(stamp_ms("2026-08-01T17:36:47.1+00:00"), None);
}

#[test]
fn a_paginated_forks_own_first_turn_counts_on_its_model() {
    let spawn = |history: &str| {
        [
            meta_as(T0, SUB, ROOT, Some(ROOT), Some(history), Some(LOGIN_A)),
            meta_as(T0, ROOT, ROOT, None, Some(history), Some(LOGIN_A)),
            context(T0, "gpt-6-luna", None),
            message(T0),
            context(T0, "gpt-6-luna", None),
            message(T0),
            context(T0 + 9, "gpt-6.1-sol", None),
            message(T0 + 10),
            count(T0 + 24, (15_565, 0, 12), (15_565, 0, 12)),
        ]
        .concat()
    };
    let got = rows(&mut TurnReader::default(), &spawn(PAGINATED));
    let t = turns(&got);
    assert_eq!(t.len(), 1, "{got:?}");
    assert_eq!(t[0].model.as_deref(), Some("gpt-6.1-sol"));
    assert_eq!(t[0].tokens.input, 15_565);
    let user_fork = [
        meta_as(T0, OTHER, OTHER, Some(ROOT), Some(PAGINATED), Some(LOGIN_A)),
        message(T0),
        context(T0 + 14_000, "gpt-6.1-sol", None),
        count(T0 + 14_010, (47_270, 0, 33), (15_860, 0, 10)),
    ]
    .concat();
    for history in [PAGINATED, "legacy"] {
        let text = user_fork.replace(PAGINATED, history);
        assert_eq!(turns(&rows(&mut TurnReader::default(), &text)).len(), 1);
    }
    let legacy = rows(&mut TurnReader::default(), &spawn("legacy"));
    assert!(turns(&legacy).is_empty(), "{legacy:?}");
    assert_eq!(legacy.last(), Some(&Row::Copied));
}

#[test]
fn a_legacy_forks_own_settings_line_ends_its_copy() {
    use super::fixtures::settings;
    let reset = T0 / 1000 + 3600;
    let prompted = |own: &str| {
        [
            meta_as(T0, SUB, SUB, Some(ROOT), Some("legacy"), Some(LOGIN_A)),
            meta(T0, ROOT, ROOT, None),
            context(T0, "gpt-6-luna", None),
            message(T0),
            count(T0, (6_000, 0, 30), (3_000, 0, 10)),
            rate(T0, "codex", Some((43.0, 300, reset)), None),
            settings(T0, ROOT),
            settings(T0, own),
            settings(T0 + 14, own),
            context(T0 + 24, "gpt-6.1-sol", None),
            message(T0 + 33),
            count(T0 + 37, (19_992, 0, 30), (13_992, 0, 0)),
            rate(T0 + 37, "codex", Some((55.0, 300, reset + 1)), None),
        ]
        .concat()
    };
    let text = prompted(SUB);
    let got = rows(&mut TurnReader::default(), &text);
    let t = turns(&got);
    assert_eq!(t.len(), 1, "{got:?}");
    assert_eq!(t[0].model.as_deref(), Some("gpt-6.1-sol"));
    assert_eq!(t[0].tokens.input, 13_992);
    assert_eq!(got[4], Row::Copied, "the copy's count");
    let mut reader = TurnReader::default();
    let (head, tail) = text.split_at(text.find(&settings(T0, ROOT)).expect("a copied line"));
    let first = rows(&mut reader, head);
    assert!(turns(&first).is_empty());
    assert_eq!(reader.thread.as_deref(), Some(SUB), "kept inside the copy");
    let second = rows(&mut reader, tail);
    assert_eq!(turns(&second).len(), 1, "{second:?}");
    assert_eq!(
        (reader.prefix, reader.thread.as_deref()),
        (Prefix::Own, None)
    );
    let dir = aterm_tempfile::Builder::new()
        .prefix("codex-usage-settings-")
        .tempdir()
        .expect("temp dir");
    let path = dir.path().join("rollout.jsonl");
    let shares = |text: &str| {
        std::fs::write(&path, text).expect("write");
        rollout_readings(&path, CONTEXT_REACH)
            .expect("read")
            .iter()
            .flat_map(|r| r.windows.iter().map(|w| w.used_percent))
            .collect::<Vec<_>>()
    };
    assert_eq!(shares(&text), [12.0, 55.0], "the own count's and reading");
    let other = prompted(OTHER);
    let got = rows(&mut TurnReader::default(), &other);
    assert!(turns(&got).is_empty(), "{got:?}");
    assert_eq!(got.last(), Some(&Row::Copied));
    assert_eq!(shares(&other), Vec::<f64>::new());
}

fn append(path: &Path, text: &str) {
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .expect("append");
}

fn written_at(path: &Path, at: i64) {
    let when = SystemTime::UNIX_EPOCH
        + std::time::Duration::from_secs(u64::try_from(at).expect("after 1970"));
    std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(when))
        .expect("set the mtime");
}

fn inputs(turns: &[Turn]) -> Vec<u64> {
    turns.iter().map(|t| t.tokens.input).collect()
}

fn step(cursor: &mut RolloutCursor, path: &Path, cap: u64) -> (RolloutRead, Vec<Turn>) {
    let mut got = Vec::new();
    let read = cursor
        .advance(path, cap, &mut |t| got.push(t.clone()))
        .expect("the rollout opens");
    (read, got)
}

#[test]
fn a_cursor_hands_each_appended_turn_once_and_a_rewrite_none_again() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("codex-cursor-")
        .tempdir()
        .expect("temp dir");
    let path = dir.path().join("rollout.jsonl");
    let head = [
        meta(T0, ROOT, ROOT, None),
        context(T0 + 1, "gpt-6-sol", None),
    ]
    .concat();
    let one = count(T0 + 2, (10, 0, 1), (10, 0, 1));
    let two = count(T0 + 3, (30, 0, 3), (20, 0, 2));
    let three = count(T0 + 4, (60, 0, 6), (30, 0, 3));
    std::fs::write(&path, format!("{head}{one}")).expect("write");
    let mut cursor = RolloutCursor::default();
    let (read, first) = step(&mut cursor, &path, u64::MAX);
    assert_eq!(inputs(&first), [10]);
    assert_eq!(read.read, (head.len() + one.len()) as u64);
    assert!(read.complete);
    append(&path, &two);
    let (read, second) = step(&mut cursor, &path, u64::MAX);
    assert_eq!(inputs(&second), [20]);
    assert_eq!(
        read.read,
        two.len() as u64 + 1,
        "the appended line and the byte before it"
    );
    let (read, third) = step(&mut cursor, &path, u64::MAX);
    assert!(third.is_empty(), "{third:?}");
    assert!(read.complete && read.read == 1, "{read:?}");
    let (torn, rest) = three.split_at(three.len() / 2);
    append(&path, torn);
    let (read, none) = step(&mut cursor, &path, u64::MAX);
    assert!(
        none.is_empty() && read.complete,
        "a torn line waits: {none:?}"
    );
    append(&path, rest);
    assert_eq!(inputs(&step(&mut cursor, &path, u64::MAX).1), [30]);
    let text = format!("{head}{one}{two}{three}");
    let rewrite = dir.path().join("rewrite.jsonl");
    std::fs::write(&rewrite, &text).expect("write");
    std::fs::rename(&rewrite, &path).expect("rename over");
    let (read, again) = step(&mut cursor, &path, u64::MAX);
    assert_eq!(
        read.read,
        text.len() as u64,
        "another identity: from its start"
    );
    assert!(again.is_empty(), "{again:?}");
    let four = count(T0 + 5, (100, 0, 10), (40, 0, 4));
    let shifted = format!(
        "{}{}{one}{two}{three}{four}",
        meta(T0, ROOT, ROOT, None),
        context(T0 + 1, "gpt-6-sol-x", None)
    );
    std::fs::write(&path, &shifted).expect("rewrite in place");
    let (read, moved) = step(&mut cursor, &path, u64::MAX);
    assert_eq!(
        read.read,
        shifted.len() as u64 + 1,
        "its line ends moved: from its start"
    );
    assert_eq!(inputs(&moved), [40], "only the turn it had not handed");
    let fresh = step(&mut RolloutCursor::default(), &path, u64::MAX).1;
    assert_eq!(
        inputs(&fresh),
        [10, 20, 30, 40],
        "NEGATIVE CONTROL: a reader that kept no total hands them all"
    );
}

#[test]
fn a_budgeted_cursor_hands_what_an_unbounded_one_does() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("codex-cursor-cap-")
        .tempdir()
        .expect("temp dir");
    let path = dir.path().join("rollout.jsonl");
    let text = [
        meta(T0, ROOT, ROOT, None),
        context(T0 + 1, "gpt-6-sol", None),
        count(T0 + 2, (10, 0, 1), (10, 0, 1)),
        message(T0 + 3),
        count(T0 + 4, (30, 0, 3), (20, 0, 2)),
        count(T0 + 5, (60, 0, 6), (30, 0, 3)),
    ]
    .concat();
    std::fs::write(&path, &text).expect("write");
    let whole = step(&mut RolloutCursor::default(), &path, u64::MAX).1;
    assert_eq!(inputs(&whole), [10, 20, 30]);
    let mut cursor = RolloutCursor::default();
    let (read, none) = step(&mut cursor, &path, 0);
    assert!(
        none.is_empty() && read.read == 0 && !read.complete,
        "a cap of 0 reads no line"
    );
    let lines = text.lines().count();
    let mut got = Vec::new();
    let mut reads = 0;
    for _ in 0..lines {
        let (read, turns) = step(&mut cursor, &path, 1);
        reads += 1;
        got.extend(turns);
        if read.complete {
            break;
        }
    }
    assert_eq!(reads, lines, "one whole line a read");
    assert_eq!(got, whole);
}

#[test]
fn rollouts_fold_hands_each_turn_once_and_lets_go_of_what_is_gone() {
    let home = aterm_tempfile::Builder::new()
        .prefix("codex-rollouts-")
        .tempdir()
        .expect("temp dir");
    let day = home.path().join("sessions/2026/11/05");
    std::fs::create_dir_all(&day).expect("mkdir");
    let now = i64::try_from(aterm_time::unix_secs().expect("a clock")).expect("seconds");
    let since = now - 3600;
    let base = (now - 600) * 1000;
    let body = |thread: &str| {
        [
            meta(base, thread, thread, None),
            context(base + 1, "gpt-6-sol", None),
            count(base + 2, (10, 0, 1), (10, 0, 1)),
            count(base + 3, (30, 0, 3), (20, 0, 2)),
        ]
        .concat()
    };
    let file = |thread: &str, at: i64| {
        let path = day.join(format!("rollout-2026-11-05T10-00-00-{thread}.jsonl"));
        std::fs::write(&path, body(thread)).expect("write");
        written_at(&path, at);
        path
    };
    let a = file(ROOT, now - 120);
    let b = file(OTHER, now - 60);
    let old = file(SUB, since - 3600);
    let fold = |rollouts: &mut Rollouts, since: i64| {
        let mut got = Vec::new();
        let caught_up = rollouts.fold(home.path(), since, u64::MAX, &mut |path, turn| {
            got.push((path.to_path_buf(), turn.tokens.input));
        });
        (caught_up, got)
    };
    let mut rollouts = Rollouts::default();
    let (caught_up, first) = fold(&mut rollouts, since);
    assert!(caught_up);
    assert_eq!(
        first,
        [
            (b.clone(), 10),
            (b.clone(), 20),
            (a.clone(), 10),
            (a.clone(), 20)
        ],
        "newest first, each turn once"
    );
    assert!(
        !rollouts.files.contains_key(&old),
        "older than `since`: not read"
    );
    let (caught_up, second) = fold(&mut rollouts, since);
    assert!(caught_up && second.is_empty(), "{second:?}");
    append(&a, &count(base + 4, (60, 0, 6), (30, 0, 3)));
    assert_eq!(fold(&mut rollouts, since).1, [(a.clone(), 30)]);
    append(&a, &count((since - 1) * 1000, (100, 0, 10), (40, 0, 4)));
    assert!(
        fold(&mut rollouts, since).1.is_empty(),
        "served before `since`"
    );
    assert_eq!(
        rollouts.files[&a].reader.top, 110,
        "NEGATIVE CONTROL: it was read"
    );
    std::fs::remove_file(&b).expect("remove");
    let (caught_up, none) = fold(&mut rollouts, since);
    assert!(caught_up && none.is_empty(), "{none:?}");
    assert!(
        !rollouts.files.contains_key(&b),
        "a rollout gone keeps no cursor"
    );
    let kept = rollouts.files[&a].clone();
    let at = std::fs::metadata(&a)
        .and_then(|m| m.modified())
        .expect("mtime");
    {
        use std::io::{Seek as _, Write as _};
        let mut f = std::fs::File::options().write(true).open(&a).expect("open");
        f.seek(std::io::SeekFrom::End(-1)).expect("seek");
        f.write_all(b"x").expect("overwrite its last newline");
        f.set_modified(at).expect("put its mtime back");
    }
    let (caught_up, none) = fold(&mut rollouts, since);
    assert!(caught_up && none.is_empty(), "{none:?}");
    assert_eq!(
        rollouts.files[&a], kept,
        "the same identity, size and mtime: not opened"
    );
    written_at(&a, now - 30);
    fold(&mut rollouts, since);
    assert!(
        rollouts.files[&a].pos < kept.pos,
        "NEGATIVE CONTROL: opened, its moved line end reads it again from its start"
    );
    let (_, all) = fold(&mut Rollouts::default(), since - 7200);
    assert_eq!(
        all.iter().filter(|(path, _)| *path == old).count(),
        2,
        "NEGATIVE CONTROL: with `since` before it, the old rollout is read: {all:?}"
    );
}

/// A directory or a dangling link named as a rollout is no rollout: the fold reads the real
/// one beside it and is caught up, where listing them would leave it short every fold.
#[cfg(unix)]
#[test]
fn an_entry_named_as_a_rollout_that_is_no_file_leaves_the_fold_caught_up() {
    let home = aterm_tempfile::Builder::new()
        .prefix("codex-rollouts-")
        .tempdir()
        .expect("temp dir");
    let day = home.path().join("sessions/2026/11/05");
    std::fs::create_dir_all(&day).expect("mkdir");
    let now = i64::try_from(aterm_time::unix_secs().expect("a clock")).expect("seconds");
    let base = (now - 600) * 1000;
    let real = day.join(format!("rollout-2026-11-05T10-00-00-{ROOT}.jsonl"));
    std::fs::write(
        &real,
        [
            meta(base, ROOT, ROOT, None),
            context(base + 1, "gpt-6-sol", None),
            count(base + 2, (10, 0, 1), (10, 0, 1)),
        ]
        .concat(),
    )
    .expect("write");
    std::fs::create_dir(day.join(format!("rollout-2026-11-05T10-00-01-{OTHER}.jsonl")))
        .expect("a directory so named");
    std::os::unix::fs::symlink(
        day.join("gone"),
        day.join(format!("rollout-2026-11-05T10-00-02-{SUB}.jsonl")),
    )
    .expect("a dangling link so named");
    let looped = day.join(format!("rollout-2026-11-05T10-00-03-{OTHER}.jsonl"));
    std::os::unix::fs::symlink(&looped, &looped).expect("a link that cannot be examined");
    let mut rollouts = Rollouts::default();
    let mut got = Vec::new();
    let caught_up = rollouts.fold(home.path(), now - 3600, u64::MAX, &mut |path, turn| {
        got.push((path.to_path_buf(), turn.tokens.input));
    });
    assert_eq!(got, [(real.clone(), 10)]);
    assert!(caught_up, "an entry that is no rollout held the fold short");
    assert_eq!(rollouts.files.len(), 1, "a cursor for the rollout alone");
}

#[test]
fn codex_home_is_codex_home_else_the_users() {
    use crate::harness::host_path::abs;
    let (home, alt, pj) = (abs("/Users//x"), abs("/alt/home"), abs("/w/pj"));
    let dot_codex = Some(Path::new(&home).join(".codex"));
    let user = Some(home.as_str());
    assert_eq!(codex_home(None, user, None), dot_codex);
    assert_eq!(
        codex_home(Some(&alt), user, None),
        Some(PathBuf::from(&alt))
    );
    assert_eq!(codex_home(Some("rel"), None, None), None);
    assert_eq!(
        codex_home(Some("rel"), user, None),
        None,
        "never another home's records"
    );
    assert_eq!(
        codex_home(Some("rel"), user, Some(Path::new(&pj))),
        Some(Path::new(&pj).join("rel"))
    );
    assert_eq!(codex_home(Some("rel"), user, Some(Path::new("pj"))), None);
    assert_eq!(
        codex_home(Some(""), user, Some(Path::new(&pj))),
        dot_codex,
        "an empty one is unset"
    );
}
