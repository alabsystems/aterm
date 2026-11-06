// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the step fold reads of a transcript, held to the code: `TRANSCRIPT_KEYS` is exactly the
//! keys its projections keep and the anchors its own source names (the closed words its arms
//! compare, each read through `word`); its notice projection reads a notice as the shared
//! conversation projection does; and a row of a kind it does not act on ([`ROW_KINDS`]), carrying
//! every key and word the table names, anywhere in a committed reduction, changes nothing it makes.
//! `lab claude-measure` rests its judge on all three: GONE and NEW-KEY hold a build to the one
//! table, and a new key or record kind on an inert row is only noted.

use std::collections::BTreeSet;

use super::*;

const FP: u64 = 0x5eed_0000_0000_0009;
const ID: Option<(u64, u64)> = Some((16_777_220, 9));
/// Where a measured build's reduction is committed.
fn measured_dir() -> std::path::PathBuf {
    aterm_tempfile::manifest_dir().join("src/harness/testdata/steps")
}

/// The transcript fixtures.
fn fixtures_dir() -> std::path::PathBuf {
    aterm_tempfile::manifest_dir().join("src/supervise/fixtures")
}

/// Every anchor the fold's own source names (`anchor!("transcript.<text>")`), read off the code
/// rather than a list beside it: the keys it reads and the closed words its arms compare, which
/// it reads only through `word`, whose words are the table's.
fn named_by_the_fold() -> BTreeSet<&'static str> {
    const SOURCE: &str = include_str!("steps.rs");
    SOURCE
        .split("anchor!(\"transcript.")
        .skip(1)
        .filter_map(|rest| rest.split_once('"').map(|(text, _)| text))
        .collect()
}

/// The table's texts.
fn table() -> BTreeSet<&'static str> {
    aterm_phase::anchors::TRANSCRIPT_KEYS
        .iter()
        .map(|a| a.text)
        .collect()
}

/// Every key the fold's projections keep and every anchor its source names is a
/// `TRANSCRIPT_KEYS` anchor, and every anchor is one of them: the table is the fold's own read
/// set, so a read added to a projection without its anchor fails here, and so does an anchor
/// nothing reads. A closed word an arm compares is read through `word`, which matches only the
/// table's words, so an arm comparing a word the table does not name never matches, and the
/// fold's own tests of that arm fail (`the_user_rows_that_ask_nothing_emit_no_prompt` holds each
/// origin kind that is no one else's prompt).
#[test]
fn the_fold_reads_what_the_transcript_keys_name() {
    let table = table();
    let mut read = transcript::step_keys();
    read.extend(named_by_the_fold());
    let unnamed: Vec<&&str> = read.difference(&table).collect();
    let unread: Vec<&&str> = table.difference(&read).collect();
    assert!(
        unnamed.is_empty() && unread.is_empty(),
        "the fold reads what TRANSCRIPT_KEYS does not name: {unnamed:?}; TRANSCRIPT_KEYS names what the fold does not read: {unread:?}"
    );
}

/// The committed Claude Code reductions: name and rows (no `#` line, no blank one).
fn reductions() -> Vec<(String, Vec<String>)> {
    let dir = measured_dir();
    let mut out: Vec<(String, Vec<String>)> = std::fs::read_dir(&dir)
        .expect("the step testdata")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".jsonl") && !n.starts_with("codex-"))
        .map(|n| {
            let text = std::fs::read_to_string(dir.join(&n)).expect("a reduction");
            let rows = text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_owned)
                .collect();
            (n, rows)
        })
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no Claude Code reduction is committed");
    out
}

/// The transcript fixtures' rows, and the corpus's Claude Code rows.
fn other_rows() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut names: Vec<std::path::PathBuf> = std::fs::read_dir(fixtures_dir())
        .expect("the fixtures")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    names.sort();
    for p in names {
        let text = std::fs::read_to_string(&p).expect("a fixture");
        let name = p
            .file_name()
            .expect("a name")
            .to_string_lossy()
            .into_owned();
        out.extend(text.lines().map(|l| (name.clone(), l.to_owned())));
    }
    out.extend(
        super::super::transcript::corpus_tests::ROWS
            .iter()
            .chain(super::super::transcript::corpus_tests::TURN_ROWS)
            .filter(|(id, _)| id.starts_with("claude/"))
            .map(|(id, row)| ((*id).to_owned(), (*row).to_owned())),
    );
    out
}

/// The fold's notice projection (`transcript::step_notice`) reads every notice of every committed
/// reduction, fixture and corpus row as the shared conversation projection the wall reader was
/// written against: the fold's narrower read loses nothing of a notice.
#[test]
fn the_fold_reads_a_notice_as_the_conversation_reader_does() {
    let mut rows = other_rows();
    for (name, rs) in reductions() {
        rows.extend(rs.into_iter().map(|r| (name.clone(), r)));
    }
    let mut notices = 0;
    for (name, row) in &rows {
        let by_fold = notice_read(row);
        let shared = transcript::conversation(row)
            .ok()
            .and_then(|v| notice_of_value(&v));
        assert_eq!(by_fold, shared, "{name}: {:?}", &row[..row.len().min(160)]);
        notices += usize::from(by_fold.is_some());
    }
    assert!(notices >= 3, "only {notices} notices read");
    let typed = rows
        .iter()
        .filter_map(|(_, r)| notice_read(r))
        .any(|n| n.status.is_some() && n.limit.is_some() && n.api_error.is_some());
    assert!(
        typed,
        "no notice with a status, a limit and a typed error was read"
    );
}

/// What the fold makes of `rows`: each step (its offset, time, state and event), what it lost and
/// dropped, its chain's end after each row and at the last take.
#[derive(Debug, PartialEq, Eq)]
struct Made {
    steps: Vec<(u64, u64, StepState, StepEvent)>,
    lost: u64,
    overflow: u64,
    ends: Vec<End>,
    end: End,
}

fn made(rows: &[&str]) -> Made {
    let mut f = StepFold::new(0, None);
    f.read_at(FP, ID, 0, false);
    let mut off = 0u64;
    let mut ends = Vec::with_capacity(rows.len());
    for r in rows {
        off += r.len() as u64 + 1;
        f.push(r.as_bytes(), off);
        ends.push(f.end());
    }
    let (a, b) = (f.take(), f.take());
    Made {
        steps: a
            .steps
            .iter()
            .chain(&b.steps)
            .map(|s| (s.pos.off, s.pos.at_ms, s.state, s.ev))
            .collect(),
        lost: a.lost + b.lost,
        overflow: a.overflow + b.overflow,
        ends,
        end: b.end,
    }
}

/// `m`, folded with a row of `len` bytes inserted before row `at` (its first byte at offset
/// `from`), as the fold without it would have made it: every offset past the row moved back by
/// its line, and the end after it dropped.
fn without_row(mut m: Made, at: usize, from: u64, len: u64) -> Made {
    let past = from + len + 1;
    for s in &mut m.steps {
        if s.0 >= past {
            s.0 -= len + 1;
        }
    }
    m.ends.remove(at);
    m
}

/// The row kinds an inserted row takes: ones Claude Code writes that the fold does not act on, a
/// kind no build has written, a `type` that is no word, and none.
const INERT_KINDS: [&str; 7] = [
    r#""type":"attachment","#,
    r#""type":"tools-used","#,
    r#""type":"permission-mode","#,
    r#""type":"file-history-snapshot","#,
    r#""type":"zz-a-kind-no-build-wrote","#,
    r#""type":7,"#,
    "",
];

/// The rest of an inserted row: every key and word the fold reads (`TRANSCRIPT_KEYS`, held by
/// [`the_inert_rows_carry_every_key_the_fold_reads`]), valued as an acted-on row would carry it (a
/// measured build and an unmeasured one, an opening parent, a call, a result, an interrupt, a
/// turn's end, a compact and its summary, a transcript-only row, a notice with its limit, usage
/// with its cache counts, a model, each origin kind), the keys 2.1.312 and 2.1.316 added on rows
/// the fold does not act on, and an escape, so the row passes the fold's prefilter.
const INERT_BODIES: [&str; 3] = [
    r#""version":"2.1.303","timestamp":"2000-01-01T10:59:00.000Z","parentUuid":null,"uuid":"00000000-0000-4000-8000-0000000000f1","promptId":"p-1","isSidechain":false,"isMeta":false,"isCompactSummary":true,"queueTranscriptOnly":true,"origin":{"kind":"human"},"subtype":"turn_duration","durationMs":7,"message":{"id":"msg_x","role":"user","model":"claude-opus-4-5","usage":{"input_tokens":9,"output_tokens":4,"cache_read_input_tokens":3,"cache_creation_input_tokens":5,"cache_creation":{"ephemeral_5m_input_tokens":2,"ephemeral_1h_input_tokens":3}},"content":[{"type":"tool_use","id":"toolu_inert1","name":"Bash","input":{}},{"type":"tool_result","tool_use_id":"toolu_inert1","is_error":true},{"type":"text","text":"[Request interrupted by user for tool use]"}]},"attachment":{"type":"deferred_tools_delta","addedNames":["x"],"removedNames":[],"digests":{}},"note":"A""#,
    r#""version":"0.0.1","timestamp":"2000-01-01T11:59:00.000Z","parentUuid":"00000000-0000-4000-8000-000000000001","isApiErrorMessage":true,"error":"rate_limit","apiError":"usage_limit_reached","apiErrorStatus":429,"quotaLimits":{"rateLimitType":"five_hour","status":"rejected"},"message":{"role":"assistant","model":"<synthetic>","content":[{"type":"text","text":"You've hit your session limit · resets 4:28am"}]},"subtype":"compact_boundary","origin":{"kind":"task-notification"},"turnOrigin":"wake","scheduledTaskId":"t-1","effort":"high","role":"system","sandboxed":true,"permissionDecision":{"decision":"accept","grantScope":"session","reasonCode":"unknown"},"toolDenialKind":"user-rejected""#,
    r#""content":"<local-command-stdout>QZP9WORD1</local-command-stdout>","subtype":"local_command","message":{"content":"/compact"},"origin":{"kind":"auto-continuation"},"isMeta":true,"version":"2.1.303","timestamp":"2000-01-01T12:59:00.000Z","kind":"user""#,
];

/// The inserted rows carry every key and word the fold reads: each `TRANSCRIPT_KEYS` text is a
/// key or a value of some body or kind, so a fold arm that reads any of them on an inert row
/// meets it there; a key added to the table that no body carries fails here.
#[test]
fn the_inert_rows_carry_every_key_the_fold_reads() {
    let mut carried = BTreeSet::new();
    for row in INERT_KINDS
        .iter()
        .flat_map(|k| INERT_BODIES.iter().map(move |b| format!("{{{k}{b}}}")))
    {
        let v: Value = aterm_json::from_str(&row).expect("an inserted row is JSON");
        words_of(&v, &mut carried);
    }
    let missing: Vec<&str> = table()
        .into_iter()
        .filter(|t| !carried.contains(*t))
        .collect();
    assert!(
        missing.is_empty(),
        "no inserted row carries what the fold reads: {missing:?}"
    );
}

/// Every key at any depth of `v`, and every string value.
fn words_of(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(m) => {
            for (k, v) in m {
                out.insert(k.clone());
                words_of(v, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|v| words_of(v, out)),
        Value::String(s) => {
            out.insert(s.clone());
        }
        _ => {}
    }
}

/// `rows` with `version` taken off every row: a reduction whose acted-on rows name no build, so
/// an inert row's build would be the only one the fold could take.
fn versionless(rows: &[String]) -> Vec<String> {
    rows.iter()
        .map(|r| {
            let mut v: Value = aterm_json::from_str(r).expect("a reduction row is JSON");
            if let Some(m) = v.as_object_mut() {
                m.remove("version");
            }
            aterm_json::to_string(&v).expect("a row serializes")
        })
        .collect()
}

/// THE LAW `lab claude-measure`'s judge notes a new key or record kind on an inert row by: a row
/// of a kind the fold does not act on, with any keys, inserted anywhere in a committed reduction
/// (and in the same reduction with no row naming its build), leaves everything the fold makes
/// byte-identical, its offsets past the row aside.
#[test]
fn an_inert_row_changes_nothing_the_fold_makes() {
    let mut cases = 0usize;
    for (name, rows) in reductions() {
        for (label, rows) in [
            ("as committed", rows.clone()),
            ("versionless", versionless(&rows)),
        ] {
            let base_rows: Vec<&str> = rows.iter().map(String::as_str).collect();
            let base = made(&base_rows);
            assert!(
                base.steps.len() > 1 && base.lost == 0,
                "{name} ({label}) folds to {} steps, {} lost",
                base.steps.len(),
                base.lost
            );
            for kind in INERT_KINDS {
                for body in INERT_BODIES {
                    let row = format!("{{{kind}{body}}}");
                    let v: Value = aterm_json::from_str(&row).expect("an inserted row is JSON");
                    let said = v.get("type").and_then(Value::as_str);
                    assert!(!said.is_some_and(acts_on), "{row} is acted on");
                    let mut from = 0u64;
                    for at in 0..=base_rows.len() {
                        let mut with = base_rows.clone();
                        with.insert(at, &row);
                        let got = without_row(made(&with), at, from, row.len() as u64);
                        assert_eq!(
                            got, base,
                            "{name} ({label}): the inert row {row} before row {at} changed what the fold made"
                        );
                        cases += 1;
                        if let Some(r) = base_rows.get(at) {
                            from += r.len() as u64 + 1;
                        }
                    }
                }
            }
        }
    }
    assert!(cases >= 100, "{cases} cases");
}
