// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::center::Outcome;
use crate::model::{Meter, WallStamp, tags};
use crate::text::char_width;
use crate::{Duration, HOLD_WARN, Instant};
use aterm_codec::pct::encode as pct_encode;

fn t0() -> Instant {
    Instant::now()
}

fn stamp(ms: u64) -> WallStamp {
    WallStamp { unix_ms: ms }
}

#[test]
fn activity_rows_compat_ignores_busy_and_lists_the_harness() {
    let now = t0();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    c.post(
        Message::new(tags::UPDATE, Severity::Info, "Updating to aterm v1")
            .line("checking the download")
            .meter(Meter::busy("45 MB"))
            .hold(Hold::Live {
                stale_after: crate::STALE_UPDATE,
            }),
        stamp(1_000),
        now,
    );
    c.post(
        Message::new(tags::HARNESS, Severity::Info, "aterm harness acting").hold(Hold::LogOnly),
        stamp(2_000),
        now,
    );
    let rows = activity_rows_compat(&c, now, &pct_encode);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(
        rows[0],
        "activity kind=update phase=live progress=- title=Updating%20to%20aterm%20v1 \
         detail=checking%20the%20download stats=45%20MB outcome=-"
    );
    assert!(
        rows[1].starts_with(
            "activity kind=harness phase=done progress=- title=aterm%20harness%20acting \
             detail= stats= outcome=ok since_ms="
        ),
        "{}",
        rows[1]
    );
}

#[test]
fn activity_rows_compat_is_byte_identical() {
    let now = t0();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    let failed = c.post(
        Message::new(tags::UPDATE, Severity::Warn, "Update failed")
            .line("could not verify the download"),
        stamp(1_000),
        now,
    );
    c.commit_rows(now, 3);
    c.post(
        Message::new(
            tags::TOOLCHAIN,
            Severity::Info,
            "Installing the ALab toolchain",
        )
        .line("trust \u{00b7} extracting")
        .meter(Meter {
            fill_permille: Some(500),
            stats: "512 MB / 1.2 GB".into(),
            ..Meter::default()
        })
        .hold(Hold::Live {
            stale_after: crate::STALE_UPDATE,
        }),
        stamp(2_000),
        now,
    );
    c.commit_rows(now, 3);
    assert_eq!(
        c.on_glass().map(|l| l.id).collect::<Vec<_>>(),
        vec![failed.id, MessageId::from_raw(2).unwrap()],
        "the Warn sits above the Info meter on glass"
    );
    let rows = activity_rows_compat(&c, now, &pct_encode);
    assert_eq!(
        rows,
        vec![
            "activity kind=toolchain phase=live progress=50/100 title=Installing%20the%20ALab%20toolchain detail=trust%20%C2%B7%20extracting stats=512%20MB%20/%201.2%20GB outcome=-",
            "activity kind=update phase=live progress=- title=Update%20failed detail=could%20not%20verify%20the%20download stats= outcome=-",
        ]
    );
    let folds = now + HOLD_WARN;
    assert!(!c.settle(folds, true).retired.is_empty());
    let later = folds + Duration::from_millis(1500);
    let rows = activity_rows_compat(&c, later, &pct_encode);
    assert_eq!(rows.len(), 2);
    assert!(
        rows[0].starts_with("activity kind=toolchain phase=live progress=50/100 "),
        "{}",
        rows[0]
    );
    assert_eq!(
        rows[1],
        "activity kind=update phase=done progress=- title=Update%20failed detail=could%20not%20verify%20the%20download stats= outcome=warn since_ms=1500"
    );
    let a = c.post(
        Message::new(tags::UPDATE, Severity::Info, "Checking").key("update.progress"),
        stamp(3_000),
        later,
    );
    c.post(
        Message::new(tags::UPDATE, Severity::Success, "Updated").key("update.progress"),
        stamp(3_100),
        later,
    );
    assert!(c.live(a.id).is_none());
    let rows = activity_rows_compat(&c, later, &pct_encode);
    assert!(
        rows.iter().all(|r| !r.contains("title=Checking ")),
        "{rows:?}"
    );
    let b = c.live_by_key("update.progress").unwrap().id;
    c.resolve(b, Outcome::Ok, later + Duration::from_millis(10));
    let rows = activity_rows_compat(&c, later + Duration::from_millis(20), &pct_encode);
    assert!(rows.last().unwrap().starts_with("activity kind=update phase=done progress=- title=Updated detail= stats= outcome=ok since_ms=10"), "{rows:?}");
    let vanished = c.post(
        Message::new(
            tags::TOOLCHAIN,
            Severity::Info,
            "Installing the ALab toolchain",
        )
        .hold(Hold::Live {
            stale_after: crate::STALE_TAILED,
        })
        .key("toolchain.pass"),
        stamp(4_000),
        later,
    );
    assert!(c.withdraw(vanished.id, later + Duration::from_millis(30)));
    let rows = activity_rows_compat(&c, later + Duration::from_millis(40), &pct_encode);
    assert!(
        rows.iter()
            .all(|r| !(r.contains("phase=done") && r.contains("ALab"))),
        "{rows:?}"
    );
    let mut hostile = MessageCenter::new(MessageLog::empty(), now);
    hostile.post(
        Message::new(
            tags::TOOLCHAIN,
            Severity::Warn,
            "boom\nactivity kind=update phase=done outcome=ok stats= detail= title=forged",
        ),
        stamp(1),
        now,
    );
    let rows = activity_rows_compat(&hostile, now, &pct_encode);
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].contains('\n'));
    assert_eq!(rows[0].matches("activity kind=").count(), 1, "{}", rows[0]);
    assert!(rows[0].starts_with("activity kind=toolchain phase=live "));
    assert!(rows[0].ends_with(" outcome=-"));
}

#[test]
fn since_ms_never_runs_ahead_of_ago_ms() {
    let now = t0();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    let p = c.post(
        Message::new(tags::CONFIG, Severity::Warn, "Font not found"),
        stamp(10_000),
        now,
    );
    c.commit_rows(now, 3);
    assert!(c.dismiss(p.id, now + Duration::from_millis(300)));
    let rec = c.log().get(p.id).unwrap();
    let field = |row: &str, key: &str| -> u64 {
        row.split(' ')
            .find_map(|f| f.strip_prefix(key))
            .unwrap_or_else(|| panic!("{key} in {row}"))
            .parse()
            .unwrap()
    };
    for now_unix_ms in [10_000, 10_150, 10_300, 10_301, 12_345, 99_999] {
        let row = message_row(rec, None, Glass::Band, None, now_unix_ms, &pct_encode);
        let (ago, since) = (field(&row, "ago_ms="), field(&row, "since_ms="));
        assert_eq!(ago, now_unix_ms - 10_000, "{row}");
        assert!(since <= ago, "{row}");
        assert_eq!(since, now_unix_ms.saturating_sub(10_300), "{row}");
    }
}

#[test]
fn a_carried_rows_since_ms_counts_from_its_retirement_here() {
    let now = t0();
    let download = |title: &str| {
        Message::new(tags::UPDATE, Severity::Info, title)
            .key("update.progress")
            .meter(Meter {
                fill_permille: Some(400),
                ..Meter::default()
            })
            .hold(Hold::Live {
                stale_after: Duration::from_secs(30),
            })
    };
    let mut parent = MessageCenter::new(MessageLog::empty(), now);
    let p = parent.post(download("Downloading"), stamp(10_000), now);
    parent.commit_rows(now, 3);
    let carry = parent.carried();
    let seed = now + Duration::from_secs(60);
    let mut child = MessageCenter::new(MessageLog::empty(), seed);
    child.seed_carried(&carry, stamp(70_000), seed);
    let retired = seed + Duration::from_secs(1);
    child.post(download("Installing"), stamp(71_000), retired);
    let rec = child.log().get(p.id).unwrap();
    assert!(rec.retired_at.is_some(), "{rec:?}");
    assert_eq!(rec.retired_unix_ms, Some(71_000));
    let row = message_row(rec, None, Glass::Band, None, 71_050, &pct_encode);
    assert!(row.contains(" ago_ms=61050 "), "{row}");
    assert!(row.ends_with(" since_ms=50"), "{row}");
}

/// A GLASS-LESS HOST'S ROWS CARRY NO `glass=` FIELD (the native app, which paints no band),
/// live or retired; a band host's same row still carries `glass=-`.
/// NEGATIVE CONTROL (scratch): printing ` glass=-` for a glass-less host fails it.
#[test]
fn a_glassless_hosts_rows_carry_no_glass_field() {
    let now = t0();
    let warn = || {
        Message::new(tags::SYSTEM, Severity::Warn, "Disk nearly full")
            .hold(Hold::For(Duration::from_secs(5)))
    };
    let mut c = MessageCenter::glassless(
        MessageLog::empty(),
        now,
        std::sync::Arc::new(crate::ConfirmationSource::new(None)),
    );
    let id = c.post(warn(), stamp(5_000), now).id;
    c.settle(now, true);
    let live = message_rows(&c, &ReadQuery::default(), 6_000, &pct_encode);
    assert!(live[0].contains(" state=held rep=1 "), "{}", live[0]);
    c.settle(now + Duration::from_secs(5), true);
    assert!(c.live(id).is_none(), "its hold ended it");
    let ended = message_rows(&c, &ReadQuery::default(), 11_000, &pct_encode);
    assert!(ended[0].contains(" state=folded rep=1 "), "{}", ended[0]);

    let mut band = MessageCenter::new(MessageLog::empty(), now);
    band.post(warn(), stamp(5_000), now);
    band.settle(now, true);
    let queued = message_rows(&band, &ReadQuery::default(), 6_000, &pct_encode);
    assert!(
        queued[0].contains(" state=held glass=- rep=1 "),
        "CONTROL: {}",
        queued[0]
    );
}

#[test]
fn message_rows_carry_state_glass_and_progress() {
    let now = t0();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    let p = c.post(
        Message::new(tags::PRIVACY, Severity::Info, "File access not confirmed")
            .line("Full Disk Access may already be enabled")
            .line("second line")
            .action(crate::model::Intent::OpenSystemPane {
                pane: "full-disk-access".into(),
            })
            .action(crate::model::Intent::NotNow {
                decision: crate::model::Decision::FileAccess,
            })
            .key("privacy.fda")
            .hold(Hold::asking(Duration::from_secs(600))),
        stamp(5_000),
        now,
    );
    c.commit_rows(now, 3);
    let rec = c.log().get(p.id).unwrap();
    let row = message_row(rec, c.live(p.id), Glass::Band, Some(0), 6_500, &pct_encode);
    assert_eq!(
        row,
        "message 1 at=5000 ago_ms=1500 tag=privacy sev=info origin=host state=held glass=0 rep=1 key=privacy.fda title=File%20access%20not%20confirmed detail=Full%20Disk%20Access%20may%20already%20be%20enabled%0Asecond%20line actions=Open%20Settings,Not%20now"
    );
    let m = c.post(
        Message::new(tags::TOOLCHAIN, Severity::Info, "Installing")
            .meter(Meter {
                fill_permille: Some(425),
                stats: "x".into(),
                ..Meter::default()
            })
            .hold(Hold::Live {
                stale_after: Duration::from_secs(30),
            }),
        stamp(5_100),
        now,
    );
    let rec = c.log().get(m.id).unwrap();
    let row = message_row(rec, c.live(m.id), Glass::Band, None, 5_100, &pct_encode);
    assert!(
        row.ends_with(" key=- title=Installing detail= actions=- progress=42/100"),
        "{row}"
    );
    assert!(row.contains(" state=live glass=- "), "{row}");
    c.dismiss(p.id, now + Duration::from_millis(250));
    let rec = c.log().get(p.id).unwrap();
    let row = message_row(rec, None, Glass::Band, None, 9_000, &pct_encode);
    assert!(row.contains(" state=dismissed glass=- "), "{row}");
    assert!(row.contains(" ago_ms=4000 "), "{row}");
    assert!(row.ends_with(" since_ms=3750"), "{row}");
    let q = parse_read_args("1 tag=privacy").unwrap();
    let picked = q.select(c.log());
    assert_eq!(picked.iter().map(|r| r.id).collect::<Vec<_>>(), vec![p.id]);
    let q = parse_read_args("since=1 live sev=info").unwrap();
    assert_eq!(
        q.select(c.log()).iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![m.id]
    );
    assert_eq!(parse_read_args("").unwrap(), ReadQuery::default());
    assert_eq!(parse_read_args("9999").unwrap().n, Some(LOG_CAP));
    for junk in [
        "0", "since=0", "since=x", "tag=Bad", "sev=loud", "wat", "live=1",
    ] {
        assert_eq!(parse_read_args(junk), Err(READ_USAGE), "{junk}");
    }
}

#[test]
fn paging_since_the_last_id_loses_nothing() {
    let now = t0();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    for i in 0..100u64 {
        c.post(
            Message::new(tags::SYSTEM, Severity::Info, format!("record {i}")).hold(Hold::LogOnly),
            stamp(i),
            now,
        );
    }
    let ids = |line: &str| -> Vec<u64> {
        parse_read_args(line)
            .unwrap()
            .select(c.log())
            .iter()
            .map(|r| r.id.raw())
            .collect()
    };
    assert_eq!(ids("64 since=10"), (11..=74).collect::<Vec<_>>());
    assert_eq!(ids("64 since=74"), (75..=100).collect::<Vec<_>>());
    assert_eq!(ids("since=10"), (11..=100).collect::<Vec<_>>());
    assert_eq!(ids(""), (37..=100).collect::<Vec<_>>());
    assert_eq!(ids("5"), (96..=100).collect::<Vec<_>>());
    assert_eq!(ids("since=100"), Vec::<u64>::new());
}

#[test]
fn notice_post_parses_the_grammar_and_never_authors_an_action() {
    let req = PostRequest::parse(
        "script sev=warn key=k.1 hold=90 Something happened -- line one\\nline two",
    )
    .unwrap();
    assert_eq!(req.tag, tags::SCRIPT);
    assert_eq!(req.severity, Severity::Warn);
    assert_eq!(req.key.as_deref(), Some("wire.k.1"));
    assert_eq!(req.hold_secs, Some(90));
    assert_eq!(req.title, "Something happened");
    assert_eq!(req.detail, vec!["line one", "line two"]);
    let msg = req.into_message();
    assert_eq!(msg.origin, Origin::Wire);
    assert!(msg.actions.is_empty());
    assert_eq!(msg.hold, Hold::For(Duration::from_secs(90)));
    assert_eq!(msg.glyph, Severity::Warn.default_glyph());
    let plain = PostRequest::parse("peers  just a title").unwrap();
    assert_eq!(plain.severity, Severity::Info);
    assert_eq!(plain.title, "just a title");
    assert!(plain.detail.is_empty());
    assert_eq!(
        plain.into_message().hold,
        Hold::LogOnly,
        "an info post is a record (design ruling 163)"
    );
    for junk in [
        "",
        "script",
        "Bad title",
        "script sev=loud t",
        "script hold=0 t",
        "script hold=121 t",
        "script hold=3600 t",
        "script hold=x t",
    ] {
        assert_eq!(PostRequest::parse(junk), Err(POST_USAGE), "{junk:?}");
    }
    let long = format!("script t -- {}", "é".repeat(WIRE_DETAIL_BYTES));
    let req = PostRequest::parse(&long).unwrap();
    assert!(req.detail[0].len() <= WIRE_DETAIL_BYTES);
    let many = format!(
        "script t -- {}",
        (0..40)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\\n")
    );
    assert_eq!(
        PostRequest::parse(&many).unwrap().detail.len(),
        DETAIL_LINES_CAP
    );
    let _ = char_width;
}

use crate::center::EchoKind;
use crate::model::{Decision, Load, Unit};
use crate::progress::Eta;
use crate::text::glass_title_fault;
use crate::{
    PROGRESS_GRACE, RATE_MIN_SPAN, STALE_WIRE, WIRE_BYTES_PER_WINDOW, WIRE_LIVE_CAP,
    WIRE_MINTS_PER_WINDOW, WIRE_PAINT_GAP, WIRE_PRESSES_PER_WINDOW,
};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn fresh(now: Instant) -> (MessageCenter, WireGate) {
    (
        MessageCenter::new(MessageLog::empty(), now),
        WireGate::default(),
    )
}

fn notice(c: &mut MessageCenter, g: &mut WireGate, line: &str, now: Instant) -> Applied {
    let req = NoticeRequest::parse(line).unwrap_or_else(|e| panic!("{line:?}: {e}"));
    apply(c, g, req, stamp(1_000), now)
}

fn wire_row(
    c: &mut MessageCenter,
    g: &mut WireGate,
    tag: Tag,
    rest: &str,
    now: Instant,
) -> Applied {
    let mut req = PostRequest::parse(&format!("script {rest}")).expect("a post");
    req.tag = tag;
    apply(c, g, NoticeRequest::Post(req), stamp(1_000), now)
}

fn id_of(a: &Applied) -> MessageId {
    let raw = a
        .reply
        .rsplit('=')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{}", a.reply));
    MessageId::from_raw(raw).unwrap()
}

fn onto_glass(c: &mut MessageCenter, now: Instant) {
    c.settle(now, true);
    c.commit_rows(now, 3);
}

#[test]
fn a_script_may_not_post_under_any_of_aterms_families() {
    for own in tags::ALL.iter().filter(|t| ***t != tags::SCRIPT) {
        let own = own.as_str();
        for line in [
            format!("post {own} sev=warn Worker crashed"),
            format!("post {own} a note"),
            format!("progress k tag={own} Building aterm"),
        ] {
            assert_eq!(
                NoticeRequest::parse(&line).err(),
                Some(OWN_TAG_REFUSAL),
                "{line}"
            );
        }
    }
    for line in [
        "post script sev=warn Worker crashed",
        "post crashes a note",
        "progress k tag=script Building aterm",
        "progress k tag=deploy Building aterm",
    ] {
        assert!(NoticeRequest::parse(line).is_ok(), "{line}");
    }
}

#[test]
fn notice_parses_every_form_and_refuses_junk() {
    let Ok(NoticeRequest::Post(p)) = NoticeRequest::parse(
        "post script sev=warn key=deploy hold=90 Deploy failed -- connection refused",
    ) else {
        panic!()
    };
    assert_eq!(p.key.as_deref(), Some("wire.deploy"));
    assert_eq!((p.severity, p.hold_secs), (Severity::Warn, Some(90)));
    assert_eq!(
        (p.title.as_str(), p.detail.clone()),
        ("Deploy failed", vec!["connection refused".to_string()])
    );
    let Ok(NoticeRequest::Progress(p)) = NoticeRequest::parse(
        "progress build pct=42.5 load=disk tag=deploy Building aterm -- 3 of 10",
    ) else {
        panic!()
    };
    assert_eq!(
        p,
        ProgressRequest {
            key: "wire.build".into(),
            tag: Tag::try_new("deploy").unwrap(),
            indicator: WireIndicator::Fill(425),
            load: Some(Load::Disk),
            title: "Building aterm".into(),
            stats: "3 of 10".into(),
        }
    );
    let Ok(NoticeRequest::Progress(p)) = NoticeRequest::parse("progress build Building aterm")
    else {
        panic!()
    };
    assert_eq!(
        (p.indicator, p.tag, p.load, p.stats.as_str()),
        (WireIndicator::Busy, tags::SCRIPT, None, "")
    );
    for (line, want) in [
        ("progress b busy Working", WireIndicator::Busy),
        ("progress b pct=0 Working", WireIndicator::Fill(0)),
        ("progress b pct=100 Working", WireIndicator::Fill(1000)),
        ("progress b pct=100.0 Working", WireIndicator::Fill(1000)),
        ("progress b pct=007 Working", WireIndicator::Fill(70)),
        (
            "progress b done=5/10 Working",
            WireIndicator::Amount {
                done: 5,
                total: 10,
                unit: Unit::Items,
            },
        ),
        (
            "progress b unit=bytes done=12/10 Working",
            WireIndicator::Amount {
                done: 12,
                total: 10,
                unit: Unit::Bytes,
            },
        ),
    ] {
        let Ok(NoticeRequest::Progress(p)) = NoticeRequest::parse(line) else {
            panic!("{line}")
        };
        assert_eq!(p.indicator, want, "{line}");
    }
    let key40 = "a._-".repeat(10);
    assert!(NoticeRequest::parse(&format!("progress {key40} Working")).is_ok());
    for (line, how, words) in [
        ("done build", DoneHow::Ok, None),
        ("done build ok", DoneHow::Ok, None),
        ("done build warn", DoneHow::Warn, None),
        ("done build withdraw", DoneHow::Withdraw, None),
        (
            "done build ok Built aterm",
            DoneHow::Ok,
            Some("Built aterm"),
        ),
    ] {
        assert_eq!(
            NoticeRequest::parse(line),
            Ok(NoticeRequest::Done(DoneRequest {
                key: "wire.build".into(),
                how,
                words: words.map(str::to_string),
            })),
            "{line}"
        );
    }
    let id7 = MessageId::from_raw(7).unwrap();
    assert_eq!(
        NoticeRequest::parse("dismiss 7"),
        Ok(NoticeRequest::Dismiss(id7))
    );
    for (line, press) in [
        ("act 7 0", Press::Index(0)),
        ("act 7 1", Press::Index(1)),
        ("act 7 Not now", Press::Label("Not now".into())),
        ("act 7 details", Press::Details),
    ] {
        assert_eq!(
            NoticeRequest::parse(line),
            Ok(NoticeRequest::Act { id: id7, press }),
            "{line}"
        );
    }
    let long_key = "k".repeat(41);
    let junk: Vec<(String, &str)> = [
        ("", NOTICE_USAGE),
        ("shout hello", NOTICE_USAGE),
        ("Post system t", NOTICE_USAGE),
        ("post", POST_USAGE),
        ("post script", POST_USAGE),
        ("post script key=Bad t", KEY_REFUSAL),
        ("post script key= t", KEY_REFUSAL),
        ("post script key=wire/x t", KEY_REFUSAL),
        ("post update t", OWN_TAG_REFUSAL),
        ("post harness t", OWN_TAG_REFUSAL),
        ("post toolchain sev=warn t", OWN_TAG_REFUSAL),
        ("post script hold=+5 t", POST_USAGE),
        ("post script -- only a detail", POST_USAGE),
        ("progress", PROGRESS_USAGE),
        ("progress k", PROGRESS_USAGE),
        ("progress k -- stats only", PROGRESS_USAGE),
        ("progress Bad t", KEY_REFUSAL),
        ("progress pct=4 t", KEY_REFUSAL),
        ("progress k pct=101 t", PROGRESS_USAGE),
        ("progress k pct=100.5 t", PROGRESS_USAGE),
        ("progress k pct=4.25 t", PROGRESS_USAGE),
        ("progress k pct=42. t", PROGRESS_USAGE),
        ("progress k pct=.5 t", PROGRESS_USAGE),
        ("progress k pct=-1 t", PROGRESS_USAGE),
        ("progress k pct=+4 t", PROGRESS_USAGE),
        ("progress k pct=1000 t", PROGRESS_USAGE),
        ("progress k pct= t", PROGRESS_USAGE),
        ("progress k done=5/0 t", PROGRESS_USAGE),
        ("progress k done=5 t", PROGRESS_USAGE),
        ("progress k done=/2 t", PROGRESS_USAGE),
        ("progress k done=1/2/3 t", PROGRESS_USAGE),
        ("progress k done=-1/2 t", PROGRESS_USAGE),
        ("progress k unit=bytes t", PROGRESS_USAGE),
        ("progress k pct=4 unit=items t", PROGRESS_USAGE),
        ("progress k done=1/2 unit=miles t", PROGRESS_USAGE),
        ("progress k pct=4 busy t", PROGRESS_USAGE),
        ("progress k pct=4 pct=5 t", PROGRESS_USAGE),
        ("progress k busy busy t", PROGRESS_USAGE),
        ("progress k load=gpu t", PROGRESS_USAGE),
        ("progress k tag=Bad t", PROGRESS_USAGE),
        ("progress k tag=update t", OWN_TAG_REFUSAL),
        ("done", DONE_USAGE),
        ("done K", KEY_REFUSAL),
        ("done k maybe", DONE_USAGE),
        ("done k Built aterm", DONE_USAGE),
        ("dismiss", DISMISS_USAGE),
        ("dismiss 0", DISMISS_USAGE),
        ("dismiss x", DISMISS_USAGE),
        ("dismiss +3", DISMISS_USAGE),
        ("dismiss 3 4", DISMISS_USAGE),
        ("act", ACT_USAGE),
        ("act 3", ACT_USAGE),
        ("act 0 details", ACT_USAGE),
        ("act x 0", ACT_USAGE),
        ("act 3 2", ACT_USAGE),
        ("act 3 999", ACT_USAGE),
        ("act 3 \u{1b}", ACT_USAGE),
    ]
    .into_iter()
    .map(|(l, e)| (l.to_string(), e))
    .chain([
        (format!("progress {long_key} t"), KEY_REFUSAL),
        (format!("done {long_key}"), KEY_REFUSAL),
    ])
    .collect();
    for (line, want) in &junk {
        assert_eq!(NoticeRequest::parse(line), Err(*want), "{line:?}");
    }
}

#[test]
fn an_info_post_is_a_record_and_a_warn_post_is_a_row() {
    let now = t0();
    let (mut c, mut g) = fresh(now);
    let a = notice(
        &mut c,
        &mut g,
        "post script hold=90 hello from a script",
        now,
    );
    assert_eq!(
        a,
        Applied {
            reply: "OK recorded message=1".into(),
            paint: Paint::None,
            note: true,
        }
    );
    let rec = c.log().get(id_of(&a)).unwrap();
    assert_eq!(rec.retired(), Some(&Retired::Recorded));
    assert_eq!(rec.origin, Origin::Wire);
    assert_eq!(c.live_rows().count(), 0, "a record is never a row");
    let s = notice(&mut c, &mut g, "post script sev=success Deployed", now);
    assert_eq!(s.reply, "OK recorded message=2");
    let w = notice(
        &mut c,
        &mut g,
        "post script sev=warn Deploy failed -- connection refused",
        now,
    );
    assert_eq!(
        (w.reply.as_str(), w.paint, w.note),
        ("OK message=3", Paint::Now, true)
    );
    let row = c.live(id_of(&w)).unwrap();
    assert_eq!(row.msg.hold, Hold::Default);
    let e = notice(
        &mut c,
        &mut g,
        "post script sev=error hold=90 Disk full",
        now,
    );
    assert_eq!(
        c.live(id_of(&e)).unwrap().msg.hold,
        Hold::For(Duration::from_secs(90))
    );
    let p = notice(&mut c, &mut g, "progress deploy Deploying aterm", now);
    let fixed = notice(
        &mut c,
        &mut g,
        "post script key=deploy Deploy fixed",
        now + ms(10),
    );
    assert_eq!(fixed.paint, Paint::Now);
    assert!(c.live(id_of(&p)).is_none());
    assert_eq!(
        c.log().get(id_of(&p)).unwrap().retired(),
        Some(&Retired::Resolved(Outcome::Ok))
    );
    assert_eq!(
        c.log().get(id_of(&fixed)).unwrap().key.as_deref(),
        Some("wire.deploy")
    );
}

#[test]
fn every_wire_row_earns_its_place_on_the_glass() {
    let now = t0();
    let (mut c, mut g) = fresh(now);
    for line in [
        "post script Just so you know",
        "post peers sev=success All good -- details here",
        "post worker sev=error key=a Worker crashed -- exit 3",
        "progress b Building aterm",
        "progress c pct=10 load=cpu Verifying the payload",
        "done a",
        "progress d done=1/9 unit=steps Migrating the store",
    ] {
        let a = notice(&mut c, &mut g, line, now);
        assert!(a.reply.starts_with("OK "), "{line}: {}", a.reply);
    }
    assert_eq!(c.log().len(), 6);
    for rec in c
        .log()
        .records()
        .filter(|r| r.key.as_deref() != Some("wire.a"))
    {
        assert_eq!(rec.origin, Origin::Wire);
        assert!(rec.actions.is_empty());
        match c.live(rec.id) {
            None => {
                assert!(rec.severity <= Severity::Info, "{rec:?}");
                assert_eq!(rec.retired(), Some(&Retired::Recorded));
            }
            Some(l) => {
                assert_eq!(glass_title_fault(&l.msg.title), None, "{}", l.msg.title);
                let indicator = l
                    .msg
                    .meter
                    .as_ref()
                    .is_some_and(|m| m.fill_permille.is_some() || m.busy);
                let progress = matches!(l.msg.hold, Hold::Live { .. }) && indicator;
                assert!(
                    progress || l.msg.severity >= Severity::Warn,
                    "an FYI on glass: {:?}",
                    l.msg
                );
                assert!(l.msg.severity != Severity::Success);
            }
        }
    }
}

#[test]
fn a_glass_title_from_the_wire_obeys_the_title_form() {
    for (line, want) in [
        (
            "post script sev=warn one two three four five six seven",
            "notice: title over 6 words; put the rest after --",
        ),
        (
            "post script sev=error Deploy failed: connection refused",
            "notice: title has a clause (\u{2014}, ;, :); put it after --",
        ),
        (
            "progress k Building the whole of the aterm workspace now",
            "notice: title over 6 words; put the rest after --",
        ),
        (
            "progress k Supercalifragilisticexpialidocious-builds everywhere",
            "notice: title over 48 characters; put the rest after --",
        ),
        (
            "progress k Building aterm.",
            "notice: title ends with a period",
        ),
        (
            "done k ok Built; shipped",
            "notice: title has a clause (\u{2014}, ;, :)",
        ),
        (
            "done k warn one two three four five six seven",
            "notice: title over 6 words",
        ),
    ] {
        assert_eq!(NoticeRequest::parse(line), Err(want), "{line}");
    }
    let long = format!("post script {}", "word ".repeat(20));
    let Ok(NoticeRequest::Post(p)) = NoticeRequest::parse(&long) else {
        panic!()
    };
    assert_eq!(p.title.chars().count(), 99);
    let over = format!("post script {}", "x".repeat(200));
    let Ok(NoticeRequest::Post(p)) = NoticeRequest::parse(&over) else {
        panic!()
    };
    assert_eq!(p.title.chars().count(), TITLE_CAP);
    assert!(NoticeRequest::parse("post script Build: done. Next; step").is_ok());
}

#[test]
fn the_wire_strips_what_could_forge_a_row() {
    let Ok(NoticeRequest::Post(p)) = NoticeRequest::parse(
        "post script sev=warn Deploy\u{1b}[31m \u{202e}failed -- a\u{7}b\\n\u{200b}\\nc",
    ) else {
        panic!()
    };
    assert_eq!(p.title, "Deploy[31m failed");
    assert_eq!(p.detail, vec!["ab", "c"]);
    let Ok(NoticeRequest::Progress(p)) =
        NoticeRequest::parse(&format!("progress k Working -- {}\u{1b}", "9".repeat(80)))
    else {
        panic!()
    };
    assert_eq!(p.stats.chars().count(), STATS_CAP);
    assert!(p.stats.chars().all(|c| !c.is_control()));
    assert_eq!(
        NoticeRequest::parse("post script \u{200b}\u{202e}"),
        Err(POST_USAGE)
    );
    assert_eq!(
        NoticeRequest::parse("progress k \u{7}\u{7}"),
        Err(PROGRESS_USAGE)
    );
    assert_eq!(
        NoticeRequest::parse("post script sev=warn Build:\u{2060} failed"),
        Err("notice: title has a clause (\u{2014}, ;, :); put it after --")
    );
    assert_eq!(
        NoticeRequest::parse("post script sev=warn Build failed.\u{2060}"),
        Err("notice: title ends with a period")
    );
    let Ok(NoticeRequest::Post(p)) = NoticeRequest::parse(
        "post script sev=warn De\u{061c}ploy\u{00ad} fa\u{e0041}il\u{fdd0}ed\u{ffff}\u{1fffe}\u{206a}\u{fff9}",
    ) else {
        panic!()
    };
    assert_eq!(p.title, "Deploy failed");
}

#[test]
fn a_tab_on_the_wire_is_a_space() {
    let Ok(NoticeRequest::Post(p)) =
        NoticeRequest::parse("post\tscript\tsev=warn\tDeploy\tfailed\t--\tconnection\x0brefused")
    else {
        panic!()
    };
    assert_eq!(p.severity, Severity::Warn);
    assert_eq!(p.title, "Deploy failed");
    assert_eq!(p.detail, vec!["connection refused"]);
    let Ok(NoticeRequest::Progress(p)) =
        NoticeRequest::parse("progress k\tpct=5\tFetching\tsources")
    else {
        panic!()
    };
    assert_eq!(p.title, "Fetching sources");
    assert_eq!(
        NoticeRequest::parse("post script sev=warn one\ttwo\tthree\tfour\tfive\tsix\tseven"),
        Err("notice: title over 6 words; put the rest after --"),
        "a tab is a word gap to the title form too"
    );
}

#[test]
fn the_wire_names_only_its_own_keys() {
    let now = t0();
    let (mut c, mut g) = fresh(now);
    let update = c.post(
        Message::new(tags::UPDATE, Severity::Info, "Downloading aterm")
            .key("update.progress")
            .meter(Meter::busy(""))
            .hold(Hold::Live {
                stale_after: crate::STALE_UPDATE,
            }),
        stamp(1),
        now,
    );
    let host_k = c.post(
        Message::new(tags::SYSTEM, Severity::Warn, "Host failure").key("k"),
        stamp(2),
        now,
    );
    let before: Vec<Live> = c.live_rows().cloned().collect();
    let p = notice(&mut c, &mut g, "progress k Working", now);
    let mine = c.live(id_of(&p)).unwrap();
    assert_eq!(mine.msg.key.as_deref(), Some("wire.k"));
    for host in &before {
        assert_eq!(c.live(host.id), Some(host), "a host row moved");
    }
    assert_eq!(
        notice(&mut c, &mut g, "progress update.progress Hijack", now).reply,
        format!("OK message={}", id_of(&p).raw() + 1),
        "wire.update.progress is the wire's own, a new row"
    );
    assert_eq!(c.live(update.id), before.iter().find(|l| l.id == update.id));
    notice(&mut c, &mut g, "post script key=k Fixed", now);
    assert!(c.live(host_k.id).is_some(), "a record resolves wire.k only");
    assert!(c.live(id_of(&p)).is_none());
    let again = notice(&mut c, &mut g, "progress k Working", now);
    let d = notice(&mut c, &mut g, "done k", now);
    assert_eq!(
        d.reply,
        format!("OK done={} how=resolved-ok", id_of(&again))
    );
    assert!(c.live(host_k.id).is_some() && c.live(update.id).is_some());
}

#[test]
fn progress_raises_one_row_per_key_and_restates_it_in_place() {
    let now = t0();
    let (mut c, mut g) = fresh(now);
    let a = notice(&mut c, &mut g, "progress build Building aterm", now);
    assert_eq!((a.paint, a.note), (Paint::Now, true));
    let id = id_of(&a);
    let row = c.live(id).unwrap();
    assert!(row.is_busy(), "no indicator word is busy");
    assert_eq!(
        row.msg.tag,
        tags::SCRIPT,
        "a script's row is not aterm's own system"
    );
    assert_eq!(row.msg.origin, Origin::Wire);
    assert_eq!(row.msg.severity, Severity::Info);
    assert_eq!(
        row.msg.hold,
        Hold::Live {
            stale_after: STALE_WIRE
        }
    );
    assert_eq!(row.msg.reveal_after, Some(PROGRESS_GRACE));
    assert!(!row.revealed, "a job done inside the grace never flashes");
    let logged = c.log().len();
    let b = notice(
        &mut c,
        &mut g,
        "progress build pct=42.5 tag=deploy Building aterm -- 3 of 10",
        now + ms(100),
    );
    assert_eq!((b.reply.as_str(), b.note), ("OK message=1", false));
    assert_eq!(c.log().len(), logged, "a restate logs nothing");
    let row = c.live(id).unwrap();
    assert_eq!(row.msg.tag, tags::SCRIPT, "the tag is the first line's");
    let m = row.msg.meter.as_ref().unwrap();
    assert_eq!(
        (m.fill_permille, m.busy, m.stats.as_str()),
        (Some(425), false, "3 of 10")
    );
    let mut t = now;
    for step in 1..=8_u64 {
        t = now + ms(200) + Duration::from_secs(step - 1);
        notice(
            &mut c,
            &mut g,
            &format!(
                "progress build done={}/100 unit=bytes Downloading aterm",
                step * 5
            ),
            t,
        );
    }
    let row = c.live(id).unwrap();
    let amount = row.msg.meter.as_ref().unwrap().amount.unwrap();
    assert_eq!(amount.series, Amount::series_of("wire.build"));
    assert_eq!((amount.done, amount.unit), (40, Unit::Bytes));
    assert!(t - now > RATE_MIN_SPAN);
    assert!(
        matches!(row.track.eta(t), Eta::Remaining(_)),
        "{:?}",
        row.track.eta(t)
    );
    let h = notice(&mut c, &mut g, "progress fetch load=network Fetching", t);
    assert_eq!(c.live(id_of(&h)).unwrap().shown_load(), Some(Load::Network));
    let f = notice(
        &mut c,
        &mut g,
        "post script sev=warn key=fetch Fetch failed",
        t,
    );
    assert!(c.live(id_of(&h)).is_none());
    assert_eq!(
        c.log().get(id_of(&h)).unwrap().retired(),
        Some(&Retired::Superseded { by: id_of(&f) })
    );
    let r = notice(&mut c, &mut g, "progress fetch Retrying the fetch", t);
    assert_ne!(id_of(&r), id_of(&f));
    assert!(c.live(id_of(&f)).is_none());
    assert!(c.live(id_of(&r)).unwrap().is_busy());
    assert_eq!(
        c.live_rows()
            .filter(|l| l.msg.key.as_deref() == Some("wire.fetch"))
            .count(),
        1
    );
    let work = id_of(&notice(&mut c, &mut g, "progress same Building aterm", t));
    let fail = notice(
        &mut c,
        &mut g,
        "post script sev=error key=same Building aterm",
        t,
    );
    assert_ne!(
        id_of(&fail),
        work,
        "the failure is not a repeat of the work"
    );
    assert!(c.live(work).is_none());
    let row = c.live(id_of(&fail)).unwrap();
    assert_eq!((row.msg.severity, row.repeats), (Severity::Error, 1));
    let again = id_of(&notice(&mut c, &mut g, "progress same Building aterm", t));
    assert_ne!(
        again,
        id_of(&fail),
        "the work is not a repeat of the failure"
    );
    assert!(c.live(id_of(&fail)).is_none());
    assert!(c.live(again).unwrap().is_busy());
}

#[test]
fn an_identical_progress_line_bumps_nothing() {
    let now = t0();
    let (mut c, mut g) = fresh(now);
    let line = "progress build pct=40 load=disk Building aterm -- 3 of 10";
    let id = id_of(&notice(&mut c, &mut g, line, now));
    let on = now + PROGRESS_GRACE;
    onto_glass(&mut c, on);
    assert_eq!(c.glass_position(id), Some(0));
    let revision = c.revision();
    let logged = c.log().len();
    let stale_at = c.live(id).unwrap().stale_at.unwrap();
    for n in 1..=50_u64 {
        let a = notice(&mut c, &mut g, line, on + ms(n * 100));
        assert_eq!(
            a,
            Applied {
                reply: format!("OK message={id}"),
                paint: Paint::None,
                note: false,
            }
        );
    }
    assert_eq!(c.revision(), revision, "N identical lines bump no revision");
    assert_eq!(c.log().len(), logged);
    assert_eq!(g.due(), None);
    let rearmed = c.live(id).unwrap().stale_at.unwrap();
    assert_eq!(rearmed, on + ms(5_000) + STALE_WIRE);
    assert!(rearmed > stale_at, "the script is alive: the cap re-arms");
}

#[test]
fn a_progress_flood_paints_at_most_once_a_frame() {
    let now = t0();
    let (mut c, mut g) = fresh(now);
    let first = notice(&mut c, &mut g, "progress build pct=0 Building aterm", now);
    assert_eq!(first.paint, Paint::Now);
    let mut paints = 1_u64;
    let mut t = now;
    for i in 1..=1000_u64 {
        t = now + ms(i);
        if g.take_due(t) {
            paints += 1;
        }
        let a = notice(
            &mut c,
            &mut g,
            &format!(
                "progress build pct={}.{} Building aterm",
                i / 10 % 101,
                i % 10
            ),
            t,
        );
        assert!(!a.note);
        if a.paint == Paint::Now {
            paints += 1;
        }
        assert!(g.due().is_none_or(|d| d <= t + WIRE_PAINT_GAP));
        if let Some(d) = g.due() {
            assert_eq!(c.frame_instant(d), d, "{i}: a paced paint off the grid");
            assert!(d > t, "{i}: a paced paint in the past");
        }
    }
    let due = g.due().expect("the last change is paced");
    assert!(g.take_due(due));
    paints += 1;
    assert_eq!(g.due(), None, "taken");
    assert!(!g.take_due(due + ms(1)));
    let bound = 1 + 1000 / WIRE_PAINT_GAP.as_millis() as u64 + 1;
    assert!(
        paints <= bound,
        "{paints} paints for 1000 lines (≤ {bound})"
    );
    assert!(paints >= 20, "{paints}: the fill still moves on the glass");
    assert!(t > now);
    assert_eq!(c.live_rows().count(), 1);
    assert_eq!(c.log().len(), 1, "one mint, one log line");
}

#[test]
fn an_abandoned_progress_row_goes_stale_and_fades() {
    let now = t0();
    let (mut c, mut g) = fresh(now);
    let line = "progress sync busy Syncing the mirror";
    let id = id_of(&notice(&mut c, &mut g, line, now));
    let on = now + PROGRESS_GRACE;
    onto_glass(&mut c, on);
    assert_eq!(c.glass_position(id), Some(0));
    notice(&mut c, &mut g, line, on + Duration::from_secs(119));
    assert!(c.settle(on + STALE_WIRE, true).retired.is_empty());
    let fade = on + Duration::from_secs(119) + STALE_WIRE;
    assert_eq!(c.deadline(true), Some(fade));
    let settled = c.settle(fade, true);
    assert_eq!(settled.retired, vec![(id, Retired::Stale)]);
    let echo = c.echoes().iter().find(|e| e.id == id).unwrap();
    assert_eq!(echo.kind, EchoKind::Vanish);
    assert_eq!(
        notice(&mut c, &mut g, "done sync", fade).reply,
        "OK done=- how=gone",
        "a set -e script does not die because its row went stale"
    );
}

#[test]
fn an_ask_is_a_classified_standing_row_the_tool_resolves() {
    let now = t0();
    let (mut c, mut g) = fresh(now);
    let line = "ask release.notary tag=release Apple agreement blocks the release -- \
                Apple's developer agreement for team X lapsed, so notarization answers 403 \
                -- Accept the current agreements at developer.apple.com/account";
    let Ok(NoticeRequest::Ask(req)) = NoticeRequest::parse(line) else {
        panic!("{line}");
    };
    assert_eq!(req.key, "wire.release.notary");
    assert_eq!(req.tag.as_str(), "release");
    assert_eq!(req.title, "Apple agreement blocks the release");
    let a = notice(&mut c, &mut g, line, now);
    assert_eq!((a.reply.as_str(), a.note), ("OK message=1", true));
    let row = c.live(id_of(&a)).expect("a live row");
    assert!(row.msg.persists(), "standing: {:?}", row.msg.hold);
    assert!(row.msg.actions.is_empty(), "the socket authors no button");
    let crate::need::Need::Now(ask) = row.msg.need() else {
        panic!("classified now: {:?}", row.msg.need());
    };
    assert!(ask.is_standing());
    assert_eq!(
        ask.then().words(),
        "Accept the current agreements at developer.apple.com/account"
    );
    assert_eq!(c.demotions(), 0, "the door keeps it");
    let again = notice(&mut c, &mut g, line, now);
    assert_eq!(again.reply, "OK message=1");
    assert_eq!(c.live_rows().count(), 1);
    let done = notice(&mut c, &mut g, "done release.notary ok", now);
    assert_eq!(done.reply, "OK done=1 how=resolved-ok");
    assert_eq!(c.live_rows().count(), 0);
}

#[test]
fn an_ask_the_door_would_not_stand_is_refused() {
    for (line, want) in [
        ("ask", ASK_USAGE),
        ("ask k", ASK_USAGE),
        ("ask k Title only", ASK_USAGE),
        ("ask k Title -- a reason only", ASK_USAGE),
        ("ask k Title -- -- a step", ASK_USAGE),
        ("ask K Title -- why -- Do it", KEY_REFUSAL),
        ("ask k tag=update Title -- why -- Do it", OWN_TAG_REFUSAL),
        ("ask k tag=Bad Title -- why -- Do it", ASK_USAGE),
        (
            "ask k Title: with a clause -- why -- Do it",
            "notice: title has a clause (\u{2014}, ;, :); put it after --",
        ),
        (
            "ask k Release blocked -- it is waiting for Apple -- Accept it",
            "notice: the reason only says something is pending; say why only the owner can close it",
        ),
        (
            "ask k Release blocked -- Apple refuses it -- Wait for Apple",
            "notice: a snooze is not an action; name the step that closes it",
        ),
        (
            "ask k Release blocked -- Apple refuses it -- Accept it or renew it",
            "notice: two actions in one step; name the one that closes it",
        ),
    ] {
        assert_eq!(NoticeRequest::parse(line).err(), Some(want), "{line}");
    }
}

#[test]
fn done_ends_a_row_with_its_finish() {
    let now = t0();
    let (mut c, mut g) = fresh(now);
    let on = now + PROGRESS_GRACE;
    let ok = id_of(&notice(
        &mut c,
        &mut g,
        "progress build pct=90 Building aterm",
        now,
    ));
    let warn = id_of(&notice(&mut c, &mut g, "progress test Testing aterm", now));
    let gone = id_of(&notice(&mut c, &mut g, "progress lint Linting aterm", now));
    onto_glass(&mut c, on);
    let a = notice(&mut c, &mut g, "done build ok Built aterm", on);
    assert_eq!(
        a,
        Applied {
            reply: format!("OK done={ok} how=resolved-ok"),
            paint: Paint::Now,
            note: true,
        }
    );
    let rec = c.log().get(ok).unwrap();
    assert_eq!(rec.title, "Built aterm", "the record says the words");
    let echo = c.echoes().iter().find(|e| e.id == ok).unwrap();
    assert_eq!(echo.kind, EchoKind::Complete);
    assert_eq!(echo.msg.finished_title(), "Built aterm");
    let w = notice(&mut c, &mut g, "done test warn", on);
    assert_eq!(w.reply, format!("OK done={warn} how=resolved-warn"));
    assert_eq!(
        c.echoes().iter().find(|e| e.id == warn).unwrap().kind,
        EchoKind::Fault
    );
    let v = notice(&mut c, &mut g, "done lint withdraw", on);
    assert_eq!(v.reply, format!("OK done={gone} how=withdrawn"));
    assert_eq!(
        c.log().get(gone).unwrap().retired(),
        Some(&Retired::Withdrawn)
    );
    assert_eq!(
        c.echoes().iter().find(|e| e.id == gone).unwrap().kind,
        EchoKind::Vanish
    );
    assert!(activity_rows_compat(&c, on, &pct_encode).is_empty());
    let none = notice(&mut c, &mut g, "done nothing", on);
    assert_eq!(
        none,
        Applied {
            reply: "OK done=- how=gone".into(),
            paint: Paint::None,
            note: false,
        }
    );
    let p = id_of(&notice(
        &mut c,
        &mut g,
        "post script sev=warn key=db Database down",
        on,
    ));
    assert_eq!(
        notice(&mut c, &mut g, "done db", on).reply,
        format!("OK done={p} how=resolved-ok")
    );
    let quick = id_of(&notice(&mut c, &mut g, "progress quick Checking", on));
    notice(&mut c, &mut g, "done quick", on + ms(500));
    assert!(c.echoes().iter().all(|e| e.id != quick));
}

#[test]
fn the_wire_caps_its_live_rows_and_its_mints() {
    let now = t0();
    let (mut c, mut g) = fresh(now);
    for k in 1..=WIRE_LIVE_CAP {
        notice(&mut c, &mut g, &format!("progress k{k} Working"), now);
    }
    let revision = c.revision();
    let logged = c.log().len();
    let cap =
        format!("ERR notice: {WIRE_LIVE_CAP} wire messages are live; end one with notice done");
    for line in ["progress k5 Working", "post script sev=warn Deploy failed"] {
        let a = notice(&mut c, &mut g, line, now);
        assert_eq!(
            a,
            Applied {
                reply: cap.clone(),
                paint: Paint::None,
                note: false,
            },
            "{line}"
        );
    }
    assert_eq!(
        (c.revision(), c.log().len()),
        (revision, logged),
        "a refusal changes nothing"
    );
    assert!(
        notice(&mut c, &mut g, "progress k1 pct=5 Working", now)
            .reply
            .starts_with("OK")
    );
    assert!(
        notice(&mut c, &mut g, "post script sev=warn key=k2 K2 failed", now)
            .reply
            .starts_with("OK message=")
    );
    assert!(
        notice(&mut c, &mut g, "post script a note", now)
            .reply
            .starts_with("OK recorded")
    );
    let (mut c, mut g) = fresh(now);
    for i in 0..10 {
        c.post(
            Message::new(tags::SYSTEM, Severity::Warn, format!("host {i}")),
            stamp(1),
            now,
        );
    }
    assert!(
        notice(&mut c, &mut g, "progress k Working", now)
            .reply
            .starts_with("OK")
    );

    let (mut c, mut g) = fresh(now);
    for i in 0..WIRE_MINTS_PER_WINDOW as u64 {
        let a = notice(
            &mut c,
            &mut g,
            &format!("post script note {i}"),
            now + ms(i),
        );
        assert!(a.reply.starts_with("OK recorded"), "{}", a.reply);
    }
    let logged = c.log().len();
    let busy = notice(&mut c, &mut g, "post script one too many", now + ms(100));
    assert_eq!(
        busy,
        Applied {
            reply: "ERR busy notice: 60 new messages a minute retry_ms=59900".into(),
            paint: Paint::None,
            note: false,
        }
    );
    let busy = notice(&mut c, &mut g, "progress k Working", now + ms(100));
    assert!(
        busy.reply.starts_with("ERR busy notice: "),
        "{}",
        busy.reply
    );
    assert_eq!(c.log().len(), logged);
    assert_eq!(c.live_rows().count(), 0);
    let later = now + Duration::from_secs(60);
    assert!(
        notice(&mut c, &mut g, "post script again", later)
            .reply
            .starts_with("OK")
    );
    assert!(
        notice(&mut c, &mut g, "post script again 2", later)
            .reply
            .starts_with("ERR busy"),
        "the second mint of 1 ms leaves at 60 s + 1 ms"
    );
    assert!(
        notice(&mut c, &mut g, "post script again 2", later + ms(1))
            .reply
            .starts_with("OK")
    );
    assert_eq!(
        notice(&mut c, &mut g, "post script again 3", later + ms(1)).reply,
        "ERR busy notice: 60 new messages a minute retry_ms=1"
    );
    let (mut c, mut g) = fresh(now);
    let w = notice(&mut c, &mut g, "post script sev=warn Deploy failed", now);
    let logged = c.log().len();
    for i in 1..=100 {
        let d = notice(
            &mut c,
            &mut g,
            "post script sev=warn Deploy failed",
            now + ms(i),
        );
        assert_eq!(d.reply, w.reply);
        assert!(!d.note);
        assert_ne!(d.paint, Paint::None);
    }
    assert_eq!(c.live(id_of(&w)).unwrap().repeats, 101);
    assert_eq!(c.log().len(), logged);
    for i in 1..WIRE_MINTS_PER_WINDOW {
        let a = notice(
            &mut c,
            &mut g,
            &format!("post script note {i}"),
            now + ms(200),
        );
        assert!(a.reply.starts_with("OK recorded"), "{i}: {}", a.reply);
    }
    assert!(
        notice(&mut c, &mut g, "post script the 61st", now + ms(200))
            .reply
            .starts_with("ERR busy")
    );
}

#[test]
fn dismiss_takes_down_what_a_person_could() {
    let now = t0();
    let (mut c, mut g) = fresh(now);
    let wire = id_of(&notice(
        &mut c,
        &mut g,
        "progress build Building aterm",
        now,
    ));
    let held = c
        .post(
            Message::new(tags::SYSTEM, Severity::Warn, "Disk nearly full"),
            stamp(1),
            now,
        )
        .id;
    let standing = c
        .post(
            Message::new(tags::RENDER, Severity::Error, "GPU lost").hold(Hold::STANDING),
            stamp(1),
            now,
        )
        .id;
    let working = c
        .post(
            Message::new(tags::UPDATE, Severity::Info, "Downloading aterm")
                .meter(Meter::busy(""))
                .hold(Hold::Live {
                    stale_after: crate::STALE_UPDATE,
                }),
            stamp(1),
            now,
        )
        .id;
    let ask = c
        .post(
            Message::new(tags::PRIVACY, Severity::Info, "Allow file access")
                .action(Intent::NotNow {
                    decision: Decision::FileAccess,
                })
                .hold(Hold::asking(Duration::from_secs(600))),
            stamp(1),
            now,
        )
        .id;
    for id in [wire, held, standing] {
        let a = notice(&mut c, &mut g, &format!("dismiss {id}"), now);
        assert_eq!(
            a,
            Applied {
                reply: format!("OK dismissed={id}"),
                paint: Paint::Now,
                note: true,
            }
        );
        assert_eq!(
            c.log().get(id).unwrap().retired(),
            Some(&Retired::Dismissed)
        );
    }
    let revision = c.revision();
    for (id, want) in [
        (
            working,
            format!("ERR notice: message {working} is work in flight"),
        ),
        (
            ask,
            format!("ERR notice: message {ask} asks; answer it with notice act"),
        ),
        (wire, format!("ERR notice: no live message {wire}")),
    ] {
        let a = notice(&mut c, &mut g, &format!("dismiss {id}"), now);
        assert_eq!((a.reply, a.paint, a.note), (want, Paint::None, false));
    }
    assert_eq!(
        notice(&mut c, &mut g, "dismiss 999", now).reply,
        "ERR notice: no live message 999"
    );
    assert_eq!(c.revision(), revision);
    assert!(c.live(working).is_some() && c.live(ask).is_some());
}

#[test]
fn resolve_press_names_indices_labels_and_details() {
    let now = t0();
    let (mut c, mut g) = fresh(now);
    let id = c
        .post(
            Message::new(tags::PRIVACY, Severity::Info, "File access not confirmed")
                .action(Intent::OpenSystemPane {
                    pane: "full-disk-access".into(),
                })
                .action(Intent::NotNow {
                    decision: Decision::FileAccess,
                })
                .hold(Hold::asking(Duration::from_secs(600))),
            stamp(1),
            now,
        )
        .id;
    let row = c.live(id).unwrap();
    for (press, want) in [
        (Press::Index(0), Some(ActionIndex(0))),
        (Press::Index(1), Some(ActionIndex(1))),
        (Press::Label("Open Settings".into()), Some(ActionIndex(0))),
        (Press::Label("Not now".into()), Some(ActionIndex(1))),
        (Press::Details, Some(ActionIndex::DETAILS)),
        (Press::Label("not now".into()), None),
        (Press::Label("Not".into()), None),
    ] {
        assert_eq!(resolve_press(row, &press), want, "{press:?}");
    }
    let one = c
        .post(
            Message::new(tags::SYSTEM, Severity::Warn, "One capsule").action(Intent::NewWindow),
            stamp(1),
            now,
        )
        .id;
    assert_eq!(resolve_press(c.live(one).unwrap(), &Press::Index(1)), None);
    assert_eq!(
        press_target(
            &c,
            &mut g,
            id,
            &Press::Label("Not now".into()),
            &pct_encode,
            now
        ),
        Ok((ActionIndex(1), "Not now"))
    );
    assert_eq!(
        press_target(&c, &mut g, id, &Press::Details, &pct_encode, now),
        Ok((ActionIndex::DETAILS, Intent::Details.label()))
    );
    assert_eq!(
        press_target(
            &c,
            &mut g,
            id,
            &Press::Label("not now".into()),
            &pct_encode,
            now
        ),
        Err(format!("ERR notice: no action not%20now on message {id}"))
    );
    assert_eq!(
        press_target(&c, &mut g, one, &Press::Index(1), &pct_encode, now),
        Err(format!("ERR notice: no action 1 on message {one}"))
    );
    let gone = MessageId::from_raw(99).unwrap();
    assert_eq!(
        press_target(&c, &mut g, gone, &Press::Details, &pct_encode, now),
        Err("ERR notice: no live message 99".to_string())
    );
    assert_eq!(press_words(&Press::Details), "details");
    assert_eq!(
        acted_reply("Not now", true, &pct_encode),
        "OK acted=Not%20now performed=1"
    );
    assert_eq!(
        acted_reply("Install now", false, &pct_encode),
        "OK acted=Install%20now performed=0"
    );
    let a = notice(&mut c, &mut g, &format!("act {id} 1"), now);
    assert_eq!((a.reply.as_str(), a.paint), (ACT_IS_THE_HOSTS, Paint::None));
    assert!(c.live(id).is_some());
}

#[test]
fn notice_act_presses_are_budgeted() {
    let now = t0();
    let (mut c, mut g) = fresh(now);
    let row = id_of(&notice(
        &mut c,
        &mut g,
        "progress build Building aterm",
        now,
    ));
    let gone = MessageId::from_raw(999).unwrap();
    for _ in 0..20 {
        assert!(press_target(&c, &mut g, gone, &Press::Details, &pct_encode, now).is_err());
        assert!(press_target(&c, &mut g, row, &Press::Index(0), &pct_encode, now).is_err());
    }
    let mut pressed = 0;
    let mut busy = Vec::new();
    for i in 0..100u64 {
        match press_target(&c, &mut g, row, &Press::Details, &pct_encode, now + ms(i)) {
            Ok(_) => pressed += 1,
            Err(line) => busy.push(line),
        }
    }
    assert_eq!(pressed, WIRE_PRESSES_PER_WINDOW);
    assert_eq!(busy.len(), 100 - WIRE_PRESSES_PER_WINDOW);
    assert_eq!(
        busy[0],
        format!(
            "ERR busy notice: {WIRE_PRESSES_PER_WINDOW} presses a minute retry_ms={}",
            60_000 - WIRE_PRESSES_PER_WINDOW
        )
    );
    let later = now + Duration::from_secs(60);
    assert!(press_target(&c, &mut g, row, &Press::Details, &pct_encode, later).is_ok());
    assert_eq!(no_live_reply(gone), "ERR notice: no live message 999");
}

#[test]
fn wire_and_host_rows_never_merge_as_duplicates() {
    let now = t0();
    let (mut c, mut g) = fresh(now);
    let host = c
        .post(
            Message::new(tags::SESSION, Severity::Error, "Keystrokes dropped"),
            stamp(1),
            now,
        )
        .id;
    onto_glass(&mut c, now);
    let before = c.live(host).unwrap().clone();
    let later = now + Duration::from_secs(5);
    let w = wire_row(
        &mut c,
        &mut g,
        tags::SESSION,
        "sev=error Keystrokes dropped",
        later,
    );
    assert_ne!(id_of(&w), host, "a new wire row, not the host's");
    assert_eq!(c.live(id_of(&w)).unwrap().msg.origin, Origin::Wire);
    let after = c.live(host).unwrap();
    assert_eq!(after.repeats, before.repeats, "the host row's count");
    assert_eq!(after.fold_at, before.fold_at, "the host row's hold");
    let (mut c, mut g) = fresh(now);
    let planted = id_of(&wire_row(
        &mut c,
        &mut g,
        tags::PRIVACY,
        "sev=warn Allow file access",
        now,
    ));
    let real = c.post(
        Message::new(tags::PRIVACY, Severity::Warn, "Allow file access"),
        stamp(2),
        now,
    );
    assert_ne!(real.id, planted);
    assert_eq!(c.live(real.id).unwrap().msg.origin, Origin::Host);
    assert_eq!(c.live(planted).unwrap().repeats, 1);
    let again = wire_row(
        &mut c,
        &mut g,
        tags::PRIVACY,
        "sev=warn Allow file access",
        now,
    );
    assert_eq!(id_of(&again), planted);
}

#[test]
fn the_wire_budgets_the_bytes_it_writes() {
    let now = t0();
    let (mut c, mut g) = fresh(now);
    let detail = vec!["x".repeat(200); 10].join("\\n");
    let line = |i: u64| format!("post script note {i:02} -- {detail}");
    let each = words_bytes(
        &PostRequest::parse(&line(0)["post ".len()..])
            .unwrap()
            .into_message(),
    );
    let fits = WIRE_BYTES_PER_WINDOW / each;
    assert!(fits < WIRE_MINTS_PER_WINDOW, "the bytes bind first");
    for i in 0..fits as u64 {
        let a = notice(&mut c, &mut g, &line(i), now + ms(i));
        assert!(a.reply.starts_with("OK recorded"), "{i}: {}", a.reply);
    }
    let logged = c.log().len();
    let busy = notice(&mut c, &mut g, &line(99), now + ms(100));
    assert_eq!(
        busy.reply,
        format!(
            "ERR busy notice: {} KiB of words a minute retry_ms=59900",
            WIRE_BYTES_PER_WINDOW / 1024
        )
    );
    assert_eq!(c.log().len(), logged, "a refusal writes nothing");
    assert!(
        notice(&mut c, &mut g, "post script ok", now + ms(100))
            .reply
            .starts_with("OK recorded")
    );
}

#[test]
fn message_rows_carry_origin_busy_and_load() {
    let now = t0();
    let (mut c, mut g) = fresh(now);
    let busy = id_of(&notice(
        &mut c,
        &mut g,
        "progress fetch load=network Fetching the index -- 3 MB",
        now,
    ));
    let fill = id_of(&notice(
        &mut c,
        &mut g,
        "progress build pct=42.5 Building aterm",
        now,
    ));
    let rec = id_of(&notice(
        &mut c,
        &mut g,
        "post peers peer said hi -- line",
        now,
    ));
    let on = now + PROGRESS_GRACE;
    onto_glass(&mut c, on);
    let rows = message_rows(&c, &ReadQuery::default(), 3_000, &pct_encode);
    assert_eq!(
        rows,
        vec![
            format!(
                "message {busy} at=1000 ago_ms=2000 tag=script sev=info origin=wire state=live glass=0 rep=1 key=wire.fetch title=Fetching%20the%20index detail= actions=- busy=1 load=network"
            ),
            format!(
                "message {fill} at=1000 ago_ms=2000 tag=script sev=info origin=wire state=live glass=1 rep=1 key=wire.build title=Building%20aterm detail= actions=- progress=42/100"
            ),
            format!(
                "message {rec} at=1000 ago_ms=2000 tag=peers sev=info origin=wire state=recorded glass=- rep=1 key=- title=peer%20said%20hi detail=line actions=- since_ms=2000"
            ),
        ]
    );
    notice(
        &mut c,
        &mut g,
        "progress build pct=50 load=disk Building aterm",
        on,
    );
    let row = |c: &MessageCenter| {
        message_rows(c, &parse_read_args("live").unwrap(), 9_000, &pct_encode)
            .into_iter()
            .find(|r| r.starts_with(&format!("message {fill} ")))
            .unwrap()
    };
    assert!(row(&c).ends_with(" progress=50/100"), "{}", row(&c));
    let shown = on + crate::LOAD_AFTER;
    c.settle(shown, true);
    assert!(
        row(&c).ends_with(" progress=50/100 load=disk"),
        "{}",
        row(&c)
    );
    notice(&mut c, &mut g, "done build withdraw", shown);
    let q = parse_read_args(&format!("since={}", fill.raw() - 1)).unwrap();
    let withdrawn = message_rows(&c, &q, 9_000, &pct_encode);
    assert!(
        withdrawn[0].contains(" origin=wire state=withdrawn glass=- "),
        "{withdrawn:?}"
    );
    notice(&mut c, &mut g, "post script sev=warn Deploy failed", shown);
    let live = message_rows(&c, &parse_read_args("live").unwrap(), 9_000, &pct_encode);
    assert_eq!(live.len(), 2, "{live:?}");
    let warn = message_rows(
        &c,
        &parse_read_args("sev=warn").unwrap(),
        9_000,
        &pct_encode,
    );
    assert_eq!(warn.len(), 1);
    assert!(
        warn[0].contains(" sev=warn origin=wire state=held "),
        "{}",
        warn[0]
    );
    let peers = message_rows(
        &c,
        &parse_read_args("tag=peers").unwrap(),
        9_000,
        &pct_encode,
    );
    assert_eq!(peers.len(), 1);
    let newest = message_rows(&c, &parse_read_args("1").unwrap(), 9_000, &pct_encode);
    assert!(newest[0].contains("title=Deploy%20failed"), "{newest:?}");
}

#[test]
fn no_capsule_is_ever_authored_from_the_wire() {
    for line in [
        "post script Not now",
        "post script sev=warn key=a Install now",
        "progress k Open Settings",
        "progress k pct=5 load=cpu Install now -- Not now",
    ] {
        match NoticeRequest::parse(line).unwrap() {
            NoticeRequest::Post(p) => assert!(p.into_message().actions.is_empty()),
            NoticeRequest::Progress(p) => {
                let r = p.restatement();
                assert_eq!(
                    r,
                    Restatement {
                        title: Some(p.title.clone()),
                        meter: Some(Some(p.meter())),
                        ..Restatement::default()
                    },
                    "a wire restatement sets the title and the meter only"
                );
                assert!(p.into_message().actions.is_empty());
            }
            other => panic!("{other:?}"),
        }
    }
}
