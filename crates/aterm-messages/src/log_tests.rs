// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::model::{Decision, tags};

/// What the decoder hands back for `line`: every word clipped to the model's caps and control-free, empty detail lines dropped.
pub(crate) fn as_decoded(line: &LogLine) -> LogLine {
    let lines = |d: &[String]| -> Vec<String> {
        d.iter()
            .map(|l| clip(l, DETAIL_LINE_CAP))
            .filter(|l| !l.is_empty())
            .take(DETAIL_LINES_CAP)
            .collect()
    };
    match line {
        LogLine::Posted(r) => LogLine::Posted(LogRecord {
            title: clip(&r.title, TITLE_CAP),
            detail: lines(&r.detail),
            key: r
                .key
                .as_deref()
                .map(|k| clip(k, KEY_CAP))
                .filter(|k| !k.is_empty()),
            ..r.clone()
        }),
        LogLine::Retired {
            id,
            how,
            unix_ms,
            title,
            detail,
            repeats,
            mark,
        } => LogLine::Retired {
            id: *id,
            how: match how {
                Retired::Answered { label } => Retired::Answered {
                    label: clip(label, TITLE_CAP),
                },
                other => other.clone(),
            },
            unix_ms: *unix_ms,
            title: clip(title, TITLE_CAP),
            detail: lines(detail),
            repeats: *repeats,
            mark: *mark,
        },
        LogLine::Acted { id, unix_ms, label } => LogLine::Acted {
            id: *id,
            unix_ms: *unix_ms,
            label: clip(label, TITLE_CAP),
        },
        other => other.clone(),
    }
}

fn posted(id: u64) -> LogRecord {
    LogRecord {
        id: MessageId::from_raw(id).unwrap(),
        stamp: WallStamp {
            unix_ms: 1_758_470_000_000 + id,
        },
        tag: tags::CRASH,
        severity: Severity::Error,
        posted_severity: Severity::Error,
        glyph: Glyph::new('\u{26a0}').unwrap(),
        title: "tab\there \\ back\nnew\rcr\u{1f}us \u{00b7} end".into(),
        detail: vec!["line one\twith tab".into(), String::new(), "three\\".into()],
        actions: vec![
            Intent::OpenPath {
                path: "/a b/c\t.log".into(),
            },
            Intent::NotNow {
                decision: Decision::FileAccess,
            },
        ],
        key: Some("crash.last=\tk".into()),
        origin: Origin::Wire,
        repeats: 1,
        state: LogState::Posted,
        retired_unix_ms: None,
        retired_at: None,
        last_action: None,
        later: false,
    }
}

#[test]
fn codec_round_trips_every_field_and_skips_junk() {
    let lines = vec![
        LogLine::Posted(posted(7)),
        LogLine::Retired {
            id: MessageId::from_raw(7).unwrap(),
            how: Retired::Superseded {
                by: MessageId::from_raw(9).unwrap(),
            },
            unix_ms: 5,
            title: "final\twords \u{1f} us".into(),
            detail: vec!["a".into(), "b\\c".into()],
            repeats: 3,
            mark: None,
        },
        LogLine::Retired {
            id: MessageId::from_raw(8).unwrap(),
            how: Retired::Answered {
                label: "Not now".into(),
            },
            unix_ms: 6,
            title: "t".into(),
            detail: vec![],
            repeats: 1,
            mark: None,
        },
        LogLine::Retired {
            id: MessageId::from_raw(8).unwrap(),
            how: Retired::Resolved(Outcome::Warn),
            unix_ms: 6,
            title: "t".into(),
            detail: vec![],
            repeats: 1,
            mark: Some((Severity::Warn, Glyph::or_fallback('\u{26a0}'))),
        },
        LogLine::Acted {
            id: MessageId::from_raw(7).unwrap(),
            unix_ms: 9,
            label: "Open log".into(),
        },
        LogLine::Dropped { count: 12 },
    ];
    for line in &lines {
        let enc = line.encode();
        assert!(enc.starts_with("m1\t") && enc.ends_with("\tend="), "{enc}");
        assert!(
            !enc.contains('\n') && !enc.contains('\r') && !enc.contains('\u{1f}'),
            "{enc}"
        );
        let back = LogLine::decode(&enc).unwrap_or_else(|e| panic!("{e}: {enc}"));
        assert_eq!(back, as_decoded(line), "{enc}");
        assert_eq!(
            LogLine::decode(&format!("{enc}\n")).unwrap(),
            as_decoded(line),
            "a trailing newline is fine"
        );
        assert_eq!(
            LogLine::decode(&back.encode()).unwrap(),
            back,
            "what the decoder hands back is the identity from then on"
        );
    }
    let enc = lines[0].encode();
    assert!(
        enc.contains("\\t") && enc.contains("\\n") && enc.contains("\\r"),
        "{enc}"
    );
    match LogLine::decode(&enc).unwrap() {
        LogLine::Posted(rec) => {
            assert_eq!(rec.origin, Origin::Wire);
            assert_eq!(rec.key.as_deref(), Some("crash.last=k"));
            assert_eq!(rec.actions.len(), 2);
            assert_eq!(rec.title, "tabhere \\ backnewcrus \u{00b7} end");
            assert_eq!(
                rec.detail,
                vec!["line onewith tab".to_string(), "three\\".into()]
            );
        }
        other => panic!("{other:?}"),
    }
    for how in [
        Retired::Folded,
        Retired::Stale,
        Retired::Unseen,
        Retired::Dismissed,
        Retired::Evicted,
        Retired::Carried,
        Retired::Withdrawn,
        Retired::Quit,
        Retired::Resolved(Outcome::Ok),
    ] {
        assert_eq!(Retired::decode(&how.encode()), Some(how));
    }
    assert_eq!(Retired::decode("answered"), None);
    assert_eq!(Retired::decode("folded:"), None);
    let mut us_inside = posted(5);
    us_inside.detail = vec!["b\u{1f}c".into()];
    match LogLine::decode(&LogLine::Posted(us_inside).encode()).unwrap() {
        LogLine::Posted(rec) => assert_eq!(rec.detail, vec!["bc".to_string()]),
        other => panic!("{other:?}"),
    }
    assert_eq!(LogLine::decode("hello"), Err(CodecError::NotM1));
    assert_eq!(
        LogLine::decode("m2\tkind=posted\tid=1"),
        Err(CodecError::NotM1)
    );
    let full = LogLine::Posted(posted(3)).encode();
    let cut = &full[..full.len() / 2];
    assert_eq!(
        LogLine::decode(cut),
        Err(CodecError::MissingField("origin")),
        "{cut}"
    );
    let retired = lines[1].encode();
    let cut = &retired[..retired.len() - 2];
    assert_eq!(LogLine::decode(cut), Err(CodecError::Truncated), "{cut}");
    assert_eq!(
        LogLine::decode(full.trim_end_matches("\tend=")),
        Err(CodecError::Truncated),
        "a complete line with no terminator was cut inside its last value"
    );
    assert_eq!(
        LogLine::decode("m1\tkind=teleport\tid=1"),
        Err(CodecError::UnknownKind)
    );
    assert_eq!(
        LogLine::decode("m1\tkind=posted\tid=0\tt=1\ttag=crash\tsev=error\ttitle=x\torigin=host"),
        Err(CodecError::BadField("id"))
    );
    assert_eq!(
        LogLine::decode("m1\tkind=posted\tid=1\tt=1\ttag=crash\tsev=error\ttitle=x\tend="),
        Err(CodecError::MissingField("origin")),
        "a required field is named before the terminator is asked for"
    );
    let long = format!("m1\tkind=dropped\tn=1\tpad={}", "x".repeat(MAX_LINE_BYTES));
    assert_eq!(LogLine::decode(&long), Err(CodecError::TooLong));
    let novel = "m1\tkind=posted\tid=4\tt=1\ttag=system\tsev=info\tglyph=\u{2699}\ttitle=x\tactions=teleport:home\u{1f}new-window\tfuture=yes\torigin=host\tend=";
    match LogLine::decode(novel).unwrap() {
        LogLine::Posted(rec) => {
            assert_eq!(rec.glyph, Glyph::FALLBACK);
            assert_eq!(rec.actions, vec![Intent::NewWindow]);
            assert_eq!(rec.detail, Vec::<String>::new());
            assert_eq!(rec.key, None);
            assert_eq!(rec.origin, Origin::Host);
        }
        other => panic!("{other:?}"),
    }

    let mut log = MessageLog::empty();
    for line in &lines {
        log.replay(line.clone());
    }
    log.replay(LogLine::Posted(posted(40)));
    assert_eq!(
        log.len(),
        2,
        "ids 7 and 40 (8's Retired had no Posted to merge into)"
    );
    let seven = log.get(MessageId::from_raw(7).unwrap()).unwrap();
    assert_eq!(
        seven.state,
        LogState::Retired(Retired::Superseded {
            by: MessageId::from_raw(9).unwrap()
        })
    );
    assert_eq!(
        seven.title, "final\twords \u{1f} us",
        "an in-memory line replays as given: the clipping is the DECODER's (the file ingress)"
    );
    assert_eq!(seven.repeats, 3);
    assert_eq!(seven.retired_at, None, "loaded from disk: not this process");
    assert_eq!(seven.last_action, Some(("Open log".to_string(), 9)));
    let forty = log.get(MessageId::from_raw(40).unwrap()).unwrap();
    assert_eq!(
        forty.state,
        LogState::Retired(Retired::Stale),
        "nobody folded it: stale"
    );
    assert_eq!(log.next_id().raw(), 41);
    log.replay(LogLine::Posted(posted(40)));
    assert_eq!(log.len(), 2, "deduped by id");
}

#[test]
fn the_vault_sync_press_reads_back_and_an_older_build_keeps_the_rest() {
    let mut rec = posted(5);
    rec.tag = tags::PRIVACY;
    rec.title = "Joining the secrets vault failed".into();
    rec.detail = vec![
        "the secrets vault did not let this Mac in".into(),
        "Apply in System State with the vault row on and the Mac settings rows off".into(),
    ];
    rec.actions = vec![Intent::ReviewVaultSync, Intent::NewWindow];
    rec.key = Some("privacy.secrets.failed".into());
    rec.later = true;
    let line = LogLine::Posted(rec.clone()).encode();
    assert!(line.contains("review-vault-sync"), "{line}");
    assert_eq!(LogLine::decode(&line), Ok(LogLine::Posted(rec.clone())));

    let older = line.replace("review-vault-sync", "unknown-kind");
    rec.actions = vec![Intent::NewWindow];
    assert_eq!(LogLine::decode(&older), Ok(LogLine::Posted(rec)));
}

#[test]
fn a_closed_records_id_posted_again_is_another_record() {
    let id = MessageId::from_raw(15).unwrap();
    let retired = |title: &str| LogLine::Retired {
        id,
        how: Retired::Folded,
        unix_ms: 9,
        title: title.into(),
        detail: vec![],
        repeats: 1,
        mark: None,
    };
    let mut log = MessageLog::empty();
    log.replay(LogLine::Posted(LogRecord {
        title: "2 keys have no effect".into(),
        ..posted(15)
    }));
    log.replay(retired("2 keys have no effect"));
    log.replay(LogLine::Posted(LogRecord {
        title: "Claude Code 2.1.283 is up to date".into(),
        ..posted(15)
    }));
    log.replay(retired("Claude Code 2.1.283 is up to date"));
    let titles: Vec<&str> = log.records().map(|r| r.title.as_str()).collect();
    assert_eq!(
        titles,
        ["2 keys have no effect", "Claude Code 2.1.283 is up to date"]
    );
    assert!(
        log.records()
            .all(|r| r.state == LogState::Retired(Retired::Folded))
    );

    let ids: Vec<u64> = log.records().map(|r| r.id.raw()).collect();
    assert_eq!(ids, [15, 16], "one id per record (ruling 270)");
    assert_eq!(log.next_id().raw(), 17);

    let mut open = MessageLog::empty();
    open.replay(LogLine::Posted(posted(15)));
    open.replay(LogLine::Posted(posted(15)));
    assert_eq!(open.len(), 1, "still open: the same record, deduped");

    let mut both = MessageLog::empty();
    both.replay(LogLine::Posted(LogRecord {
        title: "Claude Code 2.1.283 is up to date".into(),
        ..posted(15)
    }));
    both.replay(LogLine::Posted(LogRecord {
        title: "Built aterm".into(),
        ..posted(15)
    }));
    both.replay(retired("Claude Code 2.1.283 is up to date"));
    both.replay(LogLine::Acted {
        id,
        unix_ms: 10,
        label: "Copy".into(),
    });
    let words: Vec<(u64, &str, bool)> = both
        .records()
        .map(|r| {
            (
                r.id.raw(),
                r.title.as_str(),
                r.state == LogState::Retired(Retired::Folded),
            )
        })
        .collect();
    assert_eq!(
        words,
        [
            (15, "Claude Code 2.1.283 is up to date", true),
            (16, "Built aterm", false)
        ]
    );
    assert_eq!(
        both.get(MessageId::from_raw(16).unwrap())
            .and_then(|r| r.last_action.clone()),
        Some(("Copy".to_string(), 10)),
        "a later line goes to the record still open"
    );
    both.replay(LogLine::Posted(LogRecord {
        title: "Paste stopped".into(),
        ..posted(16)
    }));
    let ids: Vec<u64> = both.records().map(|r| r.id.raw()).collect();
    assert_eq!(ids, [15, 16, 17]);

    let mut file = MessageLog::empty();
    file.replay_all([
        LogLine::Posted(posted(15)),
        retired("tab\there \\ back\nnew\rcr\u{1f}us \u{00b7} end"),
        LogLine::Posted(LogRecord {
            title: "Claude Code 2.1.283 is up to date".into(),
            ..posted(15)
        }),
        LogLine::Posted(posted(16)),
    ]);
    let ids: Vec<u64> = file.records().map(|r| r.id.raw()).collect();
    assert_eq!(ids, [15, 17, 16], "16 keeps its number");
}

#[test]
fn a_delivered_retirement_goes_to_the_record_its_finished_words_name() {
    let id = MessageId::from_raw(15).unwrap();
    let delivered = |title: &str, unix_ms: u64| LogLine::Retired {
        id,
        how: Retired::Folded,
        unix_ms,
        title: title.into(),
        detail: vec![],
        repeats: 1,
        mark: Some((Severity::Success, Glyph::or_fallback('\u{2713}'))),
    };
    let load = |a: &str, b: &str, finals: [(&str, u64); 2]| {
        let mut log = MessageLog::empty();
        log.replay(LogLine::Posted(LogRecord {
            title: a.into(),
            ..posted(15)
        }));
        log.replay(LogLine::Posted(LogRecord {
            title: b.into(),
            ..posted(15)
        }));
        for (title, at) in finals {
            log.replay(delivered(title, at));
        }
        log.records()
            .map(|r| (r.id.raw(), r.title.clone(), r.retired_unix_ms))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        load(
            "Deploying site",
            "Building docs",
            [("Deployed site", 20), ("Built docs", 30)]
        ),
        [
            (15, "Deployed site".to_string(), Some(20)),
            (16, "Built docs".to_string(), Some(30)),
        ],
        "the older record retired first keeps its own words"
    );
    assert_eq!(
        load(
            "Uploading the backup",
            "Building docs",
            [
                ("Uploading the backup \u{2014} done", 20),
                ("Built docs", 30)
            ]
        ),
        [
            (
                15,
                "Uploading the backup \u{2014} done".to_string(),
                Some(20)
            ),
            (16, "Built docs".to_string(), Some(30)),
        ],
        "a `— done` title names its record too"
    );
    assert_eq!(
        load(
            "Deploying site",
            "Building docs",
            [("Deploying stopped", 20), ("Built docs", 30)]
        )[0],
        (15, "Deploying stopped".to_string(), Some(20)),
        "a stopped form names its record"
    );
    assert_eq!(
        load(
            "Deploying site",
            "Building docs",
            [("Site is live", 20), ("Deployed site", 30)]
        ),
        [
            (15, "Deployed site".to_string(), Some(30)),
            (16, "Site is live".to_string(), Some(20)),
        ]
    );
}

#[test]
fn every_line_is_shelved_with_its_records_owner() {
    let mut log = MessageLog::empty();
    let post = |log: &mut MessageLog, origin: Origin, key: Option<&str>| {
        let id = log.mint();
        log.record_posted(LogRecord {
            id,
            origin,
            key: key.map(str::to_string),
            ..posted(1)
        });
        log.record_retired(
            id,
            Retired::Recorded,
            2,
            Instant::now(),
            FinalWords {
                title: "t",
                detail: &[],
                repeats: 1,
                mark: None,
            },
        );
        log.record_acted(id, 3, "Details");
        id
    };
    let host = post(&mut log, Origin::Host, None);
    let wire = post(&mut log, Origin::Wire, None);
    let carried = post(&mut log, Origin::Carried, Some("wire.build"));
    let shelved = log.drain_shelved();
    assert_eq!(shelved.len(), 9);
    for (line, shelf) in &shelved {
        let want = if line.id() == Some(host) {
            Shelf::Host
        } else {
            assert!(line.id() == Some(wire) || line.id() == Some(carried));
            Shelf::Wire
        };
        assert_eq!(*shelf, want, "{line:?}");
    }
    for _ in 0..(PENDING_PERSIST_CAP + 1) {
        post(&mut log, Origin::Wire, None);
    }
    let shelved = log.drain_shelved();
    let count = u32::try_from((PENDING_PERSIST_CAP + 1) * 3 - PENDING_PERSIST_CAP).unwrap();
    assert_eq!(
        shelved.last(),
        Some(&(LogLine::Dropped { count }, Shelf::Host))
    );
}

#[test]
fn wire_records_never_push_aterms_own_out_of_the_ring() {
    let retire = |log: &mut MessageLog, rec: LogRecord| {
        let id = rec.id;
        log.record_posted(rec);
        log.record_retired(
            id,
            Retired::Recorded,
            0,
            Instant::now(),
            FinalWords {
                title: "t",
                detail: &[],
                repeats: 1,
                mark: None,
            },
        );
    };
    let host = |log: &mut MessageLog| {
        let id = log.mint();
        LogRecord {
            id,
            origin: Origin::Host,
            key: None,
            ..posted(1)
        }
    };
    let wire = |log: &mut MessageLog| {
        let id = log.mint();
        LogRecord {
            id,
            origin: Origin::Wire,
            key: None,
            ..posted(1)
        }
    };
    let mut log = MessageLog::empty();
    let crash = host(&mut log);
    let crash_id = crash.id;
    retire(&mut log, crash);
    for _ in 0..600 {
        let rec = wire(&mut log);
        retire(&mut log, rec);
    }
    assert_eq!(log.len(), LOG_CAP);
    assert!(log.get(crash_id).is_some(), "the host record stays");
    let mut log = MessageLog::empty();
    for _ in 0..LOG_CAP {
        let rec = host(&mut log);
        retire(&mut log, rec);
    }
    for _ in 0..600 {
        let rec = wire(&mut log);
        retire(&mut log, rec);
    }
    let wire_n = log.records().filter(|r| r.wire_owned()).count();
    assert_eq!(wire_n, WIRE_LOG_SHARE);
    assert_eq!(log.len() - wire_n, LOG_CAP - WIRE_LOG_SHARE);
    assert_eq!(log.wire_records, wire_n, "the kept count is the ring's");
    let carried = LogRecord {
        origin: Origin::Carried,
        key: Some("wire.build".into()),
        ..posted(1)
    };
    assert!(carried.wire_owned());
    let hosted = LogRecord {
        origin: Origin::Carried,
        key: Some("update.progress".into()),
        ..posted(1)
    };
    assert!(!hosted.wire_owned());
}

#[test]
fn a_wire_filing_outlives_the_wires_own_churn_inside_its_share() {
    let file = |log: &mut MessageLog, rec: LogRecord, how: Retired| {
        let id = rec.id;
        let at = rec.stamp.unix_ms;
        log.record_posted(rec);
        log.record_retired(
            id,
            how,
            at,
            Instant::now(),
            FinalWords {
                title: "t",
                detail: &[],
                repeats: 1,
                mark: None,
            },
        );
        id
    };
    let next = |log: &mut MessageLog, origin: Origin, later: bool| {
        let id = log.mint();
        LogRecord {
            id,
            origin,
            key: later.then(|| format!("wire.k{}", id.raw())),
            later,
            ..posted(id.raw())
        }
    };
    let fill_with_host = |log: &mut MessageLog| {
        for _ in 0..LOG_CAP {
            let rec = next(log, Origin::Host, false);
            file(log, rec, Retired::Recorded);
        }
    };
    let mut log = MessageLog::empty();
    fill_with_host(&mut log);
    let rec = next(&mut log, Origin::Wire, true);
    let dismissed = file(&mut log, rec, Retired::Dismissed);
    for _ in 0..600 {
        let rec = next(&mut log, Origin::Wire, false);
        file(&mut log, rec, Retired::Recorded);
    }
    assert!(log.get(dismissed).is_some(), "the owner's dismissal stays");
    let wire_n = log.records().filter(|r| r.wire_owned()).count();
    assert_eq!(wire_n, WIRE_LOG_SHARE, "the share holds");
    assert_eq!(log.wire_records, wire_n);
    let mut log = MessageLog::empty();
    fill_with_host(&mut log);
    for _ in 0..600 {
        let rec = next(&mut log, Origin::Wire, true);
        file(&mut log, rec, Retired::Dismissed);
    }
    let wire_n = log.records().filter(|r| r.wire_owned()).count();
    assert_eq!(
        wire_n, WIRE_LOG_SHARE,
        "filings never take aterm's own place"
    );
    assert_eq!(log.len() - wire_n, LOG_CAP - WIRE_LOG_SHARE);
}

#[test]
fn codec_reads_back_a_message_at_the_crates_own_caps() {
    for (name, unit) in [
        ("box-drawing", "\u{2500}"),
        ("backslash", "\\"),
        ("cjk", "\u{6f22}"),
        ("emoji", "\u{1f680}"),
    ] {
        let msg = Message::new(tags::CRASH, Severity::Error, &unit.repeat(TITLE_CAP))
            .lines((0..DETAIL_LINES_CAP).map(|_| unit.repeat(DETAIL_LINE_CAP)))
            .key(&unit.repeat(KEY_CAP))
            .action(Intent::OpenPath {
                path: format!("/{}", unit.repeat(200)),
            })
            .action(Intent::OpenSystemPane {
                pane: unit.repeat(200),
            })
            .normalized();
        let rec = LogRecord::from_posted(MessageId::FIRST, WallStamp { unix_ms: 1 }, &msg);
        let enc = LogLine::Posted(rec.clone()).encode();
        assert!(
            enc.len() <= MAX_LINE_BYTES / 2,
            "{name}: {} bytes",
            enc.len()
        );
        let back = LogLine::decode(&enc);
        assert_eq!(
            back,
            Ok(LogLine::Posted(rec)),
            "{name}: a line the encoder wrote ({} bytes) must read back whole",
            enc.len()
        );
    }
    let worst = 4 * (TITLE_CAP + DETAIL_LINES_CAP * DETAIL_LINE_CAP + KEY_CAP)
        + 2 * DETAIL_LINES_CAP
        + 8 * 1024;
    assert!(
        worst <= MAX_LINE_BYTES / 2,
        "{worst} > {}",
        MAX_LINE_BYTES / 2
    );
}

#[test]
fn a_truncated_last_field_fails_to_decode() {
    let acted = LogLine::Acted {
        id: MessageId::from_raw(7).unwrap(),
        unix_ms: 9,
        label: "Open log".into(),
    };
    let enc = acted.encode();
    let cut = &enc[..enc.len() - 4];
    let back = LogLine::decode(cut);
    assert!(
        back.is_err(),
        "a cut inside the label decodes to a different label: {back:?} from {cut:?}"
    );
    let retired = LogLine::Retired {
        id: MessageId::from_raw(7).unwrap(),
        how: Retired::Folded,
        unix_ms: 5,
        title: "t".into(),
        detail: vec![],
        repeats: 12,
        mark: None,
    };
    let enc = retired.encode();
    let back = LogLine::decode(&enc[..enc.len() - 1]);
    assert!(
        back.is_err(),
        "a cut inside rep= decodes to a different repeat count: {back:?}"
    );
    let enc = LogLine::Dropped { count: 12 }.encode();
    let back = LogLine::decode(&enc[..enc.len() - 1]);
    assert!(
        back.is_err(),
        "a cut inside n= decodes to a different count: {back:?}"
    );
}

#[test]
fn ids_keep_rising_after_replaying_a_max_id() {
    let mut log = MessageLog::empty();
    let line = format!(
        "m1\tkind=posted\tid={}\tt=1\ttag=crash\tsev=error\ttitle=x\torigin=host\tend=",
        u64::MAX
    );
    log.replay(LogLine::decode(&line).unwrap());
    assert_eq!(log.len(), 1, "the record itself is kept");
    let a = log.mint();
    let b = log.mint();
    assert_ne!(
        a, b,
        "two mints handed out the same id {a} after a replayed id at u64::MAX"
    );
    assert_eq!(a, MessageId::FIRST);
    log.raise_to(u64::MAX);
    let c = log.mint();
    let d = log.mint();
    assert!(
        b < c && c < d,
        "{b} {c} {d}: a forged carry cannot park the sequence"
    );
    log.raise_to(ID_CEILING - 1);
    assert_eq!(
        log.mint().raw(),
        ID_CEILING - 1,
        "just below the ceiling is honoured"
    );
}

#[test]
fn a_reloaded_file_merges_what_the_parent_wrote_after_the_boot() {
    let line = |l: LogLine| LogLine::decode(&l.encode()).unwrap();
    let retired = |id: u64| LogLine::Retired {
        id: MessageId::from_raw(id).unwrap(),
        how: Retired::Resolved(crate::Outcome::Ok),
        unix_ms: 1_758_470_100_000,
        title: "done".into(),
        detail: Vec::new(),
        repeats: 1,
        mark: None,
    };
    let boot_lines = vec![
        line(LogLine::Posted(posted(1))),
        line(retired(1)),
        line(LogLine::Posted(posted(2))),
        line(LogLine::Posted(posted(3))),
    ];
    let mut ring = MessageLog::empty();
    ring.replay_all(boot_lines.clone());
    assert_eq!(
        ring.get(MessageId::from_raw(2).unwrap()).unwrap().retired(),
        Some(&Retired::Stale),
        "the boot reads the parent's open row as nobody's"
    );
    let mut carried = posted(3);
    carried.title = "restated".into();
    ring.raise_to(5);
    ring.adopt_carried(carried);
    let mut own = posted(5);
    own.stamp.unix_ms = 1_758_470_900_000;
    own.title = "the successor's own".into();
    ring.record_posted(own);
    let mut disk_lines = boot_lines;
    disk_lines.push(line(retired(2)));
    disk_lines.push(line(LogLine::Posted(posted(4))));
    disk_lines.push(line(retired(4)));
    disk_lines.push(line(LogLine::Posted(posted(5))));
    let mut disk = MessageLog::empty();
    disk.replay_all(disk_lines);

    ring.merge_reloaded(&disk);
    let two = ring.get(MessageId::from_raw(2).unwrap()).unwrap();
    assert!(
        matches!(two.retired(), Some(Retired::Resolved(_))),
        "the parent's ending, not Stale: {:?}",
        two.state
    );
    assert_eq!(two.title, "done");
    let three = ring.get(MessageId::from_raw(3).unwrap()).unwrap();
    assert!(three.is_live(), "a row live here stays live");
    assert_eq!(three.title, "restated");
    assert!(
        ring.get(MessageId::from_raw(4).unwrap())
            .is_some_and(|r| matches!(r.retired(), Some(Retired::Resolved(_)))),
        "a record the parent wrote after the boot is in the history"
    );
    let fives: Vec<&LogRecord> = ring
        .records()
        .filter(|r| r.stamp == posted(5).stamp || r.title == "the successor's own")
        .collect();
    assert_eq!(fives.len(), 2, "both records numbered 5 are kept");
    assert_eq!(
        ring.get(MessageId::from_raw(5).unwrap()).unwrap().title,
        "the successor's own",
        "this process's own number stays its own"
    );
    let ids: Vec<u64> = ring.records().map(|r| r.id.raw()).collect();
    let unique: std::collections::BTreeSet<u64> = ids.iter().copied().collect();
    assert_eq!(unique.len(), ids.len(), "one id, one record: {ids:?}");
    assert!(ring.next_id().raw() > *unique.last().unwrap());
    let before: Vec<LogRecord> = ring.records().cloned().collect();
    ring.merge_reloaded(&disk);
    assert_eq!(ring.records().cloned().collect::<Vec<_>>(), before);
    assert_eq!(
        ring.pending_len(),
        1,
        "a merge queues no line: only the own post"
    );
}

#[test]
fn loaded_records_are_capped_like_posted_ones() {
    let detail: Vec<String> = (0..30)
        .map(|i| format!("{i} {}", "d".repeat(300)))
        .collect();
    let line = format!(
        "m1\tkind=posted\tid=4\tt=1\ttag=system\tsev=info\ttitle={}\\n\\t\u{1b}[2J\tdetail={}\u{1f}\u{1f}\\t\tkey={}\torigin=host\tend=",
        "a".repeat(5000),
        detail.join("\u{1f}"),
        "k".repeat(60)
    );
    let LogLine::Posted(rec) = LogLine::decode(&line).unwrap() else {
        panic!()
    };
    assert!(
        rec.title.chars().count() <= TITLE_CAP,
        "a loaded title is {} chars (cap {TITLE_CAP})",
        rec.title.chars().count()
    );
    assert!(
        rec.title.chars().all(|c| !c.is_control()),
        "a loaded title carries control characters"
    );
    assert_eq!(rec.detail.len(), DETAIL_LINES_CAP);
    assert!(
        rec.detail
            .iter()
            .all(|l| l.chars().count() <= DETAIL_LINE_CAP)
    );
    assert!(
        rec.detail.iter().all(|l| !l.is_empty()),
        "empty and control-only lines go"
    );
    assert_eq!(rec.key.as_ref().map(|k| k.chars().count()), Some(KEY_CAP));
    let retired = format!(
        "m1\tkind=retired\tid=4\thow=answered:{}\tt=2\ttitle={}\tdetail=\trep=1\tend=",
        "l".repeat(500),
        "t".repeat(500)
    );
    match LogLine::decode(&retired).unwrap() {
        LogLine::Retired { how, title, .. } => {
            assert_eq!(title.chars().count(), TITLE_CAP);
            assert!(
                matches!(how, Retired::Answered { label } if label.chars().count() == TITLE_CAP)
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_lone_empty_detail_line_round_trips() {
    let msg = Message::new(tags::SYSTEM, Severity::Info, "t").line("");
    assert_eq!(msg.detail, Vec::<String>::new(), "dropped at ingress");
    let line = LogLine::Posted(LogRecord::from_posted(
        MessageId::FIRST,
        WallStamp { unix_ms: 1 },
        &msg,
    ));
    assert_eq!(LogLine::decode(&line.encode()).unwrap(), line);
    let mut literal = posted(2);
    literal.detail = vec![String::new()];
    let back = LogLine::decode(&LogLine::Posted(literal.clone()).encode()).unwrap();
    assert_eq!(back, as_decoded(&LogLine::Posted(literal)));
    match back {
        LogLine::Posted(rec) => assert_eq!(rec.detail, Vec::<String>::new()),
        other => panic!("{other:?}"),
    }
    assert_eq!(Retired::decode("superseded:+5"), None);
    assert_eq!(
        LogLine::decode("m1\tkind=dropped\tn=+2\tend="),
        Err(CodecError::BadField("n"))
    );
    assert_eq!(
        LogLine::decode("m1\tkind=dropped\tn=1\tn=3\tend="),
        Ok(LogLine::Dropped { count: 3 })
    );
    assert_eq!(crate::Intent::decode("apply-update:+7"), None);
    assert_eq!(crate::Hold::decode("for:+10"), None);
}
