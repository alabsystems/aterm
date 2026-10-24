// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the supervisor ledger over scripted hosts and journal files.

use std::collections::{BTreeMap, VecDeque};

use super::super::blocks::View;
use super::super::prompt::fixtures::{composer, rows};
use super::super::report::{Marker, Report};
use super::*;

/// A scripted host: replies per verb, in order; every request recorded.
#[derive(Default)]
struct Host {
    replies: BTreeMap<String, VecDeque<CtlReply>>,
    requests: Vec<String>,
}

impl Host {
    fn on(mut self, verb: &str, reply: CtlReply) -> Self {
        self.replies
            .entry(verb.to_string())
            .or_default()
            .push_back(reply);
        self
    }
}

impl Ctl for Host {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        self.requests.push(args.join(" "));
        let verb = args
            .iter()
            .find(|a| !a.starts_with('@'))
            .copied()
            .unwrap_or("");
        Ok(self
            .replies
            .get_mut(verb)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| err("unknown verb (try: help)")))
    }
}

fn ok(stdout: &str) -> CtlReply {
    CtlReply {
        code: 0,
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

fn err(text: &str) -> CtlReply {
    CtlReply {
        code: 1,
        stdout: String::new(),
        stderr: format!("aterm-ctl: ERR {text}\n"),
    }
}

const EPOCH: i64 = 1_789_000_000_000;
const WORKER: &str = "s-work";
const MANAGER: &str = "s-mgr";

fn history() -> CtlReply {
    let mut out = String::new();
    for (id, started, dur, status, text) in [
        (
            1u64,
            10_000u64,
            1_800u64,
            "settled",
            "Go on the amended lever",
        ),
        (2, 100_000, 6_005, "timeout", "Second turn, while you work"),
        (3, 200_000, 1_750, "settled", "Third turn: stop and report"),
    ] {
        out.push_str(&format!(
            "turn {id} submitted=1 status={status} started_ms={started} dur_ms={dur} seq=9 \
             hash=0000000000000000 arch=7730:{} text={}\n",
            100 + id,
            aterm_codec::pct::encode(text)
        ));
    }
    ok(&out)
}

fn screen() -> Vec<String> {
    let mut r = rows(&[
        "❯ Third turn: stop and report",
        "⏺ Done.",
        "",
        "✻ Cooked for 4s · done 2:41 PM",
        "",
    ]);
    r.extend(composer("  ? for shortcuts"));
    r
}

fn offscreen() -> CtlReply {
    offscreen_of(&[
        "⏺ Older words.",
        "❯ Go on the amended lever",
        "⏺ Working on it.",
        "  Ran 2 shell commands",
        "❯ Second turn, while you work",
        "⏺ More.",
    ])
}

fn offscreen_of(archived: &[&str]) -> CtlReply {
    let archived = rows(archived);
    let screen = screen();
    let n = archived.len() + screen.len();
    let mut stdout = String::new();
    for r in archived.iter().chain(&screen) {
        stdout.push_str(r);
        stdout.push('\n');
    }
    CtlReply {
        code: 0,
        stdout,
        stderr: format!(
            "aterm-ctl: OK {n} first=101 last={} lost=0 breaks=0 back=0 epoch=1 origin=7730 \
             alt=1 seq=900 screen_rows={}\n",
            100 + archived.len(),
            screen.len()
        ),
    }
}

fn inbox() -> CtlReply {
    CtlReply {
        code: 0,
        stdout: format!(
            "msg 1 off=4 t=205000 from={WORKER}@n-1 kind=report trust=agent len=2009 more=1\n\
             msg 2 off=5 t=206000 from=s-other@n-1 kind=note trust=agent len=12\n\
             post 9 to=@{WORKER} kind=task off=- len=64\n"
        ),
        stderr: "aterm-ctl: OK 2 hold=0 holder=- seen=2 bus_head=5 dropped=0 pending=0\n"
            .to_string(),
    }
}

fn journal_file(tag: &str, lines: &[String]) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = crate::supervise::test_scratch_path("ledger-journal", tag);
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let path = dir.join("journal.jsonl");
    let mut text = String::new();
    for l in lines {
        text.push_str(l);
        text.push('\n');
    }
    std::fs::write(&path, text).expect("write journal");
    (dir, path)
}

fn record(t: i64, line: &str) -> String {
    JournalRecord::of_line(t, Some(WORKER), line, None).to_json()
}

fn journal_lines() -> Vec<String> {
    let mut v: Vec<String> = [
        (
            20_000,
            "EVENT idle seq=5 complete=1 rows=689 ⏺ Done with the lever.",
        ),
        (30_000, "APPROVED seq=6 git log --oneline -5"),
        (
            110_000,
            "EVENT prompt seq=8 kind=bash classify=read-only command=ls -la",
        ),
        (
            150_000,
            "EVENT question seq=9 ⏺ Keep the harness or rewrite it?",
        ),
        (160_000, "DISMISSED survey seq=10"),
        (170_000, "EVENT context seq=11 9% until auto-compact"),
        (180_000, "EVENT compacted seq=12"),
        (
            190_000,
            "RECONNECT await seq 9 failed: server closed the connection",
        ),
        (195_000, "RECONNECTED after 1834 ms"),
        (
            210_000,
            "EVENT idle seq=13 complete=0 rows=12 ⏺ Stopped at the gate.",
        ),
        (220_000, "TIMEOUT"),
    ]
    .iter()
    .map(|(t, line)| record(EPOCH + t, line))
    .collect();
    v.push(
        JournalRecord::of_line(
            EPOCH + 40_000,
            Some("s-other"),
            "EVENT idle seq=1 ⏺ Hi.",
            None,
        )
        .to_json(),
    );
    v.push("{\"t\":1789000040000,\"kind\":\"eve".to_string());
    v
}

const SKEW: i64 = 40;

fn log_turns_reply(restated: bool) -> CtlReply {
    let t = |ms: i64| EPOCH + ms + SKEW;
    let ledger1 = if restated { " ledger=1" } else { "" };
    ok(&format!(
        "gap why=evicted lost=9 resync_to=20\n\
         1 open - spawn by=aterm src=aterm conf=exact t={}\n\
         2 open r1 agent agent=claude by=- src=status conf=strong t={}\n\
         24 close t1 turn out=done dur_ms=1800{ledger1} by=ctl src=screen conf=strong t={}\n\
         27 close t2 turn out=done dur_ms=900 by=human src=screen conf=strong t={}\n\
         30 open t3 turn par=r1 ledger=2 by=ctl src=screen conf=strong t={}\n\
         41 close t3 turn out=done dur_ms=6005 ledger=2 by=ctl src=screen conf=strong t={}\n\
         50 open t4 turn par=r1 ledger=3 by=ctl src=screen conf=strong t={}\n\
         52 close t4 turn out=done dur_ms=1750 ledger=3 by=ctl src=screen conf=strong t={}\n\
         60 open t5 turn par=r1 by=human src=screen conf=strong t={}\n",
        t(0),
        t(1),
        t(10_000 + 1_800),
        t(50_000),
        t(100_000),
        t(106_005),
        t(200_000),
        t(201_750),
        t(250_000),
    ))
}

fn log_mail_reply() -> CtlReply {
    let t = |ms: i64| EPOCH + ms + SKEW;
    ok(&format!(
        "7 mark - mail dir=out kind=task to=@{WORKER} inbox=8 by=- src=fabric conf=exact t={}\n\
         8 mark - mail dir=in kind=report from={WORKER}@n-1 off=4 inbox=1 by=bridge src=fabric \
         conf=exact t={}\n\
         9 mark - mail dir=in kind=note from=s-other@n-1 off=5 inbox=2 by=bridge src=fabric \
         conf=exact t={}\n\
         10 mark - mail dir=out kind=task to=@{WORKER} inbox=9 by=- src=fabric conf=exact t={}\n\
         11 mark - mail dir=out kind=note to=@s-other inbox=10 by=- src=fabric conf=exact t={}\n",
        t(5_000),
        t(205_000),
        t(206_000),
        t(209_000),
        t(209_500),
    ))
}

/// A host that answers every verb the ledger asks, its log restating each close's
/// `ledger=` when `restated`.
fn host_with(restated: bool) -> Host {
    Host::default()
        .on("whoami", ok(&format!("OK {MANAGER} deadbeef owner\n")))
        .on("history", history())
        .on("offscreen", offscreen())
        .on("inbox", inbox())
        .on("log", log_turns_reply(restated))
        .on("log", log_mail_reply())
}

fn whole_host() -> Host {
    host_with(true)
}

/// A host whose log cannot be read: every other verb answers.
fn logless_host() -> Host {
    Host::default()
        .on("whoami", ok(&format!("OK {MANAGER} deadbeef owner\n")))
        .on("history", history())
        .on("offscreen", offscreen())
        .on("inbox", inbox())
        .on("log", err("unknown verb (try: help)"))
        .on("log", err("unknown verb (try: help)"))
}

fn read(host: &mut Host, opts: &LedgerOpts) -> Ledger {
    let h = LedgerHost {
        now_ms: EPOCH + 300_000,
        tz_offset_s: 0,
    };
    gather(host, opts, &h)
}

fn opts(journal: Option<std::path::PathBuf>) -> LedgerOpts {
    LedgerOpts {
        worker: format!("@{WORKER}"),
        journal,
        since_ms: None,
    }
}

#[test]
fn the_ledger_joins_the_four_sources() {
    let (dir, path) = journal_file("whole", &journal_lines());
    let mut host = whole_host();
    let l = read(&mut host, &opts(Some(path.clone())));
    assert_eq!(
        host.requests,
        [
            "whoami",
            "@s-work history",
            "@s-work log class=turn",
            "@s-work offscreen tail=20000 max=20000 screen=1",
            "@self inbox --peek --meta",
            "@self log kinds=mail",
        ]
    );
    assert_eq!(l.worker, WORKER);
    assert_eq!(l.manager.as_deref(), Some(MANAGER));
    assert!(l.aligned);
    let clock = l.sources.iter().find(|s| s.name == "clock").expect("row");
    assert!(
        clock.ok && clock.detail.contains("no clock anchor"),
        "{}",
        clock.detail
    );
    type Placed = (u64, Option<i64>, Result<usize, String>, Option<i64>);
    let turns: Vec<Placed> = l
        .turns
        .iter()
        .map(|t| (t.id, t.t, t.rows.clone(), t.stopped))
        .collect();
    assert_eq!(
        turns,
        [
            (1, Some(EPOCH + SKEW + 10_000), Ok(3), Some(EPOCH + 20_000)),
            (
                2,
                Some(EPOCH + SKEW + 100_000),
                Ok(2),
                Some(EPOCH + 150_000)
            ),
            (
                3,
                Some(EPOCH + SKEW + 200_000),
                Ok(4),
                Some(EPOCH + 210_000)
            ),
        ]
    );
    assert_eq!(l.journal.len(), 11);
    let journal = l
        .sources
        .iter()
        .find(|s| s.name == "journal")
        .expect("a journal source");
    assert!(journal.ok, "{}", journal.detail);
    assert_eq!(
        l.mail,
        [
            Mail {
                t: Some(EPOCH + SKEW + 5_000),
                dir: Dir::Out,
                kind: "task".to_string(),
                len: None,
                trust: None,
                pending: false,
            },
            Mail {
                t: Some(EPOCH + SKEW + 205_000),
                dir: Dir::In,
                kind: "report".to_string(),
                len: Some(2009),
                trust: Some("agent".to_string()),
                pending: false,
            },
            Mail {
                t: Some(EPOCH + SKEW + 209_000),
                dir: Dir::Out,
                kind: "task".to_string(),
                len: None,
                trust: None,
                pending: true,
            },
        ]
    );
    let s = l.summary();
    assert_eq!((s.turns, s.settled, s.timeouts, s.carried), (3, 2, 1, 0));
    assert_eq!(s.turn_ms, 1_800 + 6_005 + 1_750);
    assert_eq!(
        (s.working_turns, s.working_ms),
        (3, 10_000 + 50_000 + 10_000 - 3 * SKEW)
    );
    assert_eq!(s.latencies, [80_000 + SKEW, 50_000 + SKEW]);
    assert_eq!(
        (s.median_ms(), s.max_ms(), s.waiting),
        (Some(65_000 + SKEW), Some(80_000 + SKEW), 1)
    );
    assert_eq!((s.approvals, s.dismissals), (1, 1));
    assert_eq!((s.context, s.compacted, s.reconnects), (1, 1, 1));
    assert_eq!((s.mail_in, s.mail_out), (1, 2));
    assert_eq!((s.timeouts_seen, s.exits), (1, 0));
    let items = l.items();
    let times: Vec<Option<i64>> = items.iter().map(|i| i.t).collect();
    let mut sorted = times.clone();
    sorted.sort();
    assert_eq!(times, sorted);
    assert_eq!(items.len(), 3 * 2 + 11 + 3);
    for id in 1..=3 {
        assert!(
            items
                .iter()
                .any(|i| i.what.starts_with(&format!("turn {id} began: "))),
            "turn {id} is placed where the log saw it begin"
        );
    }
    let detail = &l
        .sources
        .iter()
        .find(|s| s.name == "log")
        .expect("a log source")
        .detail;
    assert!(
        detail.starts_with(
            "@s-work log class=turn: 3 of 3 turn(s) placed by ledger= (1 by its close: the \
             open was evicted)"
        ) && detail.contains("the ring evicted 9 record(s)")
            && detail.contains(
                "; 1 closed turn(s) whose open was evicted name no ledger= (no `turn` exchange \
                 opened them, or an older aterm closed them): joined to nothing"
            )
            && !detail.contains("no longer holds"),
        "{detail}"
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

#[test]
fn the_md_form_is_tables() {
    let (dir, path) = journal_file("md", &journal_lines());
    let mut host = whole_host();
    let mut l = read(&mut host, &opts(Some(path.clone())));
    l.turns[0].text = "a | b".to_string();
    let md = render_md(&l);
    assert!(md.contains("a \\| b"), "a worker's pipe is escaped:\n{md}");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

#[test]
fn the_html_form_is_one_self_contained_page() {
    let (dir, path) = journal_file("html", &journal_lines());
    let mut host = whole_host();
    let mut l = read(&mut host, &opts(Some(path.clone())));
    l.turns[0].text = "</script><img src=http://evil/x> & <b>bold</b>".to_string();
    let html = render(&l, Format::Html);
    assert_eq!(super::super::ledger_html::is_self_contained(&html), Ok(()));
    assert_eq!(well_formed(&html), Ok(()));
    assert!(html.starts_with("<!doctype html>"), "no doctype");
    assert!(
        html.contains("&lt;/script&gt;&lt;img src=http://evil/x&gt; &amp; &lt;b&gt;bold&lt;/b&gt;"),
        "the worker's own tags are text, not markup"
    );
    assert!(
        html.contains("\\u003c/script\\u003e"),
        "the data is escaped"
    );
    assert_eq!(html.matches("</script>").count(), 2, "one close per script");
    for want in ["\"manager\"", "\"watcher\"", "\"worker\"", "\"fabric\""] {
        assert!(html.contains(want), "the html is missing lane {want}");
    }
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

fn well_formed(html: &str) -> Result<(), String> {
    const VOID: &[&str] = &[
        "meta", "br", "hr", "img", "input", "link", "source", "!doctype",
    ];
    let bytes: Vec<char> = html.chars().collect();
    let mut stack: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != '<' {
            i += 1;
            continue;
        }
        let close = bytes.get(i + 1) == Some(&'/');
        let start = i + 1 + usize::from(close);
        let mut end = start;
        while end < bytes.len() && bytes[end] != '>' {
            end += 1;
        }
        if end >= bytes.len() {
            return Err("a tag never closes".to_string());
        }
        let tag: String = bytes[start..end]
            .iter()
            .take_while(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        i = end + 1;
        if VOID.contains(&tag.as_str()) {
            continue;
        }
        if close {
            match stack.pop() {
                Some(open) if open == tag => {}
                Some(open) => return Err(format!("</{tag}> closes <{open}>")),
                None => return Err(format!("</{tag}> closes nothing")),
            }
            continue;
        }
        stack.push(tag.clone());
        if tag == "script" || tag == "style" {
            let want = format!("</{tag}>");
            let rest: String = bytes[i..].iter().collect();
            let at = rest
                .find(&want)
                .ok_or_else(|| format!("<{tag}> never closes"))?;
            i += rest[..at].chars().count() + want.chars().count();
            stack.pop();
        }
    }
    if let Some(open) = stack.pop() {
        return Err(format!("<{open}> never closes"));
    }
    Ok(())
}

#[test]
fn a_missing_source_is_named_and_the_rest_prints() {
    let (dir, path) = journal_file("missing", &journal_lines());
    let mut host = Host::default();
    let l = read(&mut host, &opts(Some(path.clone())));
    let misses: Vec<&str> = l.sources.iter().filter(|s| !s.ok).map(|s| s.name).collect();
    assert_eq!(misses, ["history", "log", "mail"]);
    assert!(l.turns.is_empty() && l.mail.is_empty());
    assert_eq!(l.journal.len(), 11);
    let mut host = whole_host();
    let l = read(&mut host, &opts(None));
    assert!(!l.journaled && !l.journal_unreadable);
    assert_eq!(l.turns.len(), 3);
    assert!(l.turns.iter().all(|t| t.stopped.is_none()));
    let l = read(
        &mut whole_host(),
        &opts(Some(dir.join("nothing-here.jsonl"))),
    );
    assert!(!l.journaled && l.journal_unreadable);
    let journal = l.sources.iter().find(|s| s.name == "journal").expect("row");
    assert!(!journal.ok, "{}", journal.detail);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

#[test]
fn without_the_log_no_working_time_is_claimed() {
    let (dir, path) = journal_file("logless-spans", &journal_lines());
    let mut host = logless_host();
    let l = read(&mut host, &opts(Some(path.clone())));
    assert!(!l.aligned);
    assert!(
        l.turns.iter().all(|t| t.t.is_none() && t.stopped.is_none()),
        "a turn the log did not place has no stop"
    );
    let s = l.summary();
    assert_eq!(
        (s.working_turns, s.working_ms),
        (0, 0),
        "no span across two clocks is a duration"
    );
    assert!(
        s.latencies.is_empty() && s.waiting == 0,
        "no latency claimed"
    );
    let log = l.sources.iter().find(|s| s.name == "log").expect("row");
    assert!(!log.ok, "{}", log.detail);
    let text = render_text(&l);
    assert!(
        !text.contains("working"),
        "the render claims a working span it could not measure:\n{text}"
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

#[test]
fn a_reply_the_archive_lost_says_so() {
    let mut host = whole_host();
    let mut off = offscreen();
    off.stdout = screen().iter().map(|r| format!("{r}\n")).collect();
    off.stderr = format!(
        "aterm-ctl: OK {} first=400 last=399 lost=4 breaks=0 back=0 epoch=1 origin=9999 alt=1 \
         seq=900 screen_rows={}\n",
        screen().len(),
        screen().len()
    );
    host.replies
        .insert("offscreen".to_string(), VecDeque::from(vec![off]));
    let l = read(&mut host, &opts(None));
    let why: Vec<String> = l
        .turns
        .iter()
        .map(|t| t.rows.clone().err().unwrap_or_else(|| "-".to_string()))
        .collect();
    assert_eq!(
        why,
        [
            "another archive (aterm restarted since)",
            "another archive (aterm restarted since)",
            "another archive (aterm restarted since)"
        ]
    );
    let mut host = Host::default()
        .on("whoami", ok(&format!("OK {MANAGER} deadbeef owner\n")))
        .on("history", history());
    let l = read(&mut host, &opts(None));
    assert!(
        l.turns
            .iter()
            .all(|t| t.rows == Err("no archive read".to_string()))
    );
    let src = l
        .sources
        .iter()
        .find(|s| s.name == "offscreen")
        .expect("row");
    assert!(!src.ok, "{}", src.detail);
}

#[test]
fn a_ledger_an_update_could_not_carry_is_not_blamed_on_the_person() {
    let lost = CtlReply {
        code: 0,
        stdout: String::new(),
        stderr: "aterm-ctl: OK 0 unheld_below=48 (history: no results)\n".to_string(),
    };
    assert_eq!(history_floor(&lost), Some(48));
    let mut host = Host::default()
        .on("whoami", ok(&format!("OK {MANAGER} deadbeef owner\n")))
        .on("history", lost);
    let l = read(&mut host, &opts(None));
    let src = l.sources.iter().find(|s| s.name == "history").expect("row");
    assert!(!src.ok && src.detail.contains("48"), "{}", src.detail);
    assert_eq!(
        history_floor(&ok("OK 2 unheld_below=7\nturn 9 x\n")),
        Some(7)
    );
    assert_eq!(history_floor(&history()), None);
}

#[test]
fn since_drops_what_came_before_it() {
    let (dir, path) = journal_file("since", &journal_lines());
    let mut host = whole_host();
    let l = read(
        &mut host,
        &LedgerOpts {
            since_ms: Some(EPOCH + 160_000),
            ..opts(Some(path.clone()))
        },
    );
    assert_eq!(l.turns.iter().map(|t| t.id).collect::<Vec<_>>(), [3]);
    assert!(l.journal.iter().all(|r| r.t >= EPOCH + 160_000));
    assert_eq!(l.journal.len(), 7);
    assert!(
        l.mail
            .iter()
            .all(|m| m.t.is_some_and(|t| t >= EPOCH + 160_000))
    );
    assert_eq!(l.mail.len(), 2);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

#[test]
fn a_malformed_since_is_refused_not_a_panic() {
    assert_eq!(parse_since("4294967295-01-01", 0), None);
    assert_eq!(parse_since("4294967295-12-31T23:59:59Z", 0), None);
    assert_eq!(parse_since("2026-09-14T10:30+1\u{e9}1", 0), None);
    assert!(parse_since("2026-09-14T10:30+0130", 0).is_some());
    assert!(parse_since("9999-01-01", 0).is_some());
}

#[test]
fn since_takes_ms_or_a_time_word() {
    const DAY: i64 = 1_789_344_000_000;
    assert_eq!(parse_since("1789344000000", 0), Some(DAY));
    assert_eq!(parse_since("2026-09-14", 0), Some(DAY));
    assert_eq!(
        parse_since("2026-09-14", -7 * 3600),
        Some(DAY + 7 * 3_600_000)
    );
    assert_eq!(parse_since("2026-09-14T00:00:00Z", -7 * 3600), Some(DAY));
    assert_eq!(parse_since("2026-09-14T10:30", 0), Some(DAY + 37_800_000));
    assert_eq!(
        parse_since("2026-09-14 10:30:05", 0),
        Some(DAY + 37_805_000)
    );
    assert_eq!(
        parse_since("2026-09-14T10:30:00-07:00", 0),
        Some(DAY + 37_800_000 + 7 * 3_600_000)
    );
    assert_eq!(
        parse_since("2026-09-14T10:30:00+0200", 0),
        Some(DAY + 37_800_000 - 2 * 3_600_000)
    );
    for bad in [
        "",
        "yesterday",
        "2026-09",
        "2026-13-01",
        "2026-09-14T25:00",
        "2026-09-14T10",
        "2026-09-14T10:30:00Q",
        "-1",
        "2026-09-14T10:30:00:00",
    ] {
        assert_eq!(parse_since(bad, 0), None, "{bad}");
    }
}

#[test]
fn a_history_record_is_read_whole() {
    let body = "OK 2\n\
        turn 7 submitted=0 status=timeout started_ms=1197729 dur_ms=1761 seq=32979 \
        hash=d06718987e6891ec by=ctl arch=1789083498272557936:185 carried=1 \
        text=Manager%20here.%20Go\n\
        turn 8 submitted=1 status=settled started_ms=2 dur_ms=3 seq=4 hash=0 arch=7730:9 \
        text=next\n";
    let turns = parse_history_records(body);
    assert_eq!(
        turns,
        [
            HistTurn {
                id: 7,
                submitted: false,
                status: "timeout".to_string(),
                by: "ctl".to_string(),
                started_ms: 1_197_729,
                dur_ms: 1_761,
                arch: Some(Mark {
                    origin: Some(1_789_083_498_272_557_936),
                    index: 185
                }),
                carried: true,
                text: "Manager here. Go".to_string(),
            },
            HistTurn {
                id: 8,
                submitted: true,
                status: "settled".to_string(),
                by: "-".to_string(),
                started_ms: 2,
                dur_ms: 3,
                arch: Some(Mark {
                    origin: Some(7730),
                    index: 9
                }),
                carried: false,
                text: "next".to_string(),
            },
        ]
    );
}

#[test]
fn the_harness_turns_are_not_the_managers() {
    let with = |by: &str| {
        let mut body = history().stdout;
        body.push_str(&format!(
            "turn 4 submitted=1 status=pressed started_ms=250000 dur_ms=0 seq=9 \
             hash=0000000000000000 by={by} arch=7730:105 text=keep%20going\n"
        ));
        let mut host = whole_host();
        host.replies
            .insert("history".to_string(), VecDeque::from(vec![ok(&body)]));
        read(&mut host, &opts(None))
    };
    let ids = |l: &Ledger| l.turns.iter().map(|t| t.id).collect::<Vec<u64>>();
    let history_detail = |l: &Ledger| {
        l.sources
            .iter()
            .find(|s| s.name == "history")
            .expect("row")
            .detail
            .clone()
    };
    let l = with("harness");
    assert_eq!(ids(&l), [1, 2, 3]);
    assert!(
        l.turns.iter().all(|t| t.swallowed.is_empty()),
        "{:#?}",
        l.turns
    );
    assert_eq!(l.summary().turns, 3);
    let detail = history_detail(&l);
    assert!(
        detail.starts_with("3 turn(s) from @s-work")
            && detail.contains("1 more typed by aterm's window harness (by=harness)"),
        "{detail}"
    );

    let l = with("ctl");
    assert_eq!(ids(&l), [1, 2, 3, 4]);
    assert_eq!(l.turns[2].swallowed, [4]);
    assert_eq!(l.summary().turns, 4);
    assert!(!history_detail(&l).contains("harness"), "{l:#?}");
}

/// The drive's carry-on after a relaunch (`history`'s `by=harness`) is the harness's turn, not
/// the manager's; a `by=ctl` turn is the manager's.
#[test]
fn the_drives_carry_on_turn_reads_as_the_harness_not_the_manager() {
    let with = |by: &str| {
        let mut body = history().stdout;
        body.push_str(&format!(
            "turn 4 submitted=1 status=settled started_ms=250000 dur_ms=0 seq=9 \
             hash=0000000000000000 by={by} arch=7730:105 text={}\n",
            aterm_codec::pct::encode(crate::harness::upgrade::CARRY_ON)
        ));
        let mut host = whole_host();
        host.replies
            .insert("history".to_string(), VecDeque::from(vec![ok(&body)]));
        read(&mut host, &opts(None))
    };
    let ids = |l: &Ledger| l.turns.iter().map(|t| t.id).collect::<Vec<u64>>();
    let history_detail = |l: &Ledger| {
        l.sources
            .iter()
            .find(|s| s.name == "history")
            .expect("row")
            .detail
            .clone()
    };
    let l = with("harness");
    assert_eq!(ids(&l), [1, 2, 3], "{:#?}", l.turns);
    assert_eq!(l.summary().turns, 3);
    let detail = history_detail(&l);
    assert!(
        detail.starts_with("3 turn(s) from @s-work")
            && detail.contains("1 more typed by aterm's window harness (by=harness)"),
        "{detail}"
    );
    assert!(
        render_text(&l).contains("1 more typed by aterm's window harness (by=harness)"),
        "SOURCES says so"
    );
    let l = with("ctl");
    assert_eq!(
        ids(&l),
        [1, 2, 3, 4],
        "NEGATIVE CONTROL: as ctl it is the manager's"
    );
    assert_eq!(l.summary().turns, 4);
    assert!(!history_detail(&l).contains("harness"), "{l:#?}");
}

#[test]
fn a_report_renders_its_view() {
    let report = Report {
        reasons: vec![],
        marker: Marker::Ledger,
        turn: Some(42),
        rows: rows(&[
            "❯ Go",
            "⏺ Bash(ls)",
            "  ⎿  a",
            "⏺ Last words.",
            "",
            "✻ Cooked for 4s · done 2:41 PM",
        ]),
        archived: 3,
        screen: 3,
        last: None,
    };
    assert_eq!(report.render_view(View::All), report.render());
    assert_eq!(
        report.render_view(View::Final),
        format!(
            "{} view=final kept=3\n--\n⏺ Last words.\n\n✻ Cooked for 4s · done 2:41 PM\n",
            report.header()
        )
    );
    assert_eq!(
        report.render_view(View::Messages),
        format!(
            "{} view=messages kept=5\n--\n❯ Go\n\n⏺ Last words.\n\n✻ Cooked for 4s · done 2:41 PM\n",
            report.header()
        )
    );
}

#[test]
fn a_turn_whose_row_is_missing_is_named_in_the_span_that_holds_it() {
    let mut host = whole_host();
    let off = offscreen_of(&[
        "⏺ Older words.",
        "❯ Go on the amended lever",
        "⏺ Working on it.",
        "  Ran 2 shell commands",
        "⏺ More.",
    ]);
    host.replies
        .insert("offscreen".to_string(), VecDeque::from(vec![off]));
    let l = read(&mut host, &opts(None));
    assert_eq!(l.turns[0].rows, Ok(4), "turn 2's rows are inside turn 1's");
    assert_eq!(l.turns[0].swallowed, [2]);
    assert!(l.turns[1].rows.is_err());
    assert!(l.turns[2].rows.is_ok() && l.turns[2].swallowed.is_empty());
    let mut host = whole_host();
    let archived = rows(&[
        "⏺ Older words.",
        "❯ Go on the amended lever",
        "⏺ Working on it.",
    ]);
    let mut screen = rows(&["⏺ Still going.", ""]);
    screen.extend(composer("  ? for shortcuts"));
    let off = CtlReply {
        code: 0,
        stdout: archived
            .iter()
            .chain(&screen)
            .map(|r| format!("{r}\n"))
            .collect(),
        stderr: format!(
            "aterm-ctl: OK {} first=101 last=103 lost=0 breaks=0 back=0 epoch=1 origin=7730 \
             alt=1 seq=900 screen_rows={}\n",
            archived.len() + screen.len(),
            screen.len()
        ),
    };
    host.replies
        .insert("offscreen".to_string(), VecDeque::from(vec![off]));
    let l = read(&mut host, &opts(None));
    assert_eq!(l.turns[0].swallowed, [2, 3]);
    assert!(l.turns[1].rows.is_err() && l.turns[2].rows.is_err());
}

#[test]
fn a_close_without_its_ledger_loses_the_join() {
    let log = read(&mut host_with(false), &opts(None));
    assert_eq!(
        log.turns.iter().map(|t| t.id).collect::<Vec<_>>(),
        [1, 2, 3],
        "the rows still read"
    );
    assert_eq!(log.turns[0].t, None, "turn 1 is not joined by time");
    assert_eq!(log.turns[1].t, Some(EPOCH + SKEW + 100_000));
    let detail = &log
        .sources
        .iter()
        .find(|s| s.name == "log")
        .expect("a log source")
        .detail;
    assert!(
        detail.starts_with("@s-work log class=turn: 2 of 3 turn(s) placed by ledger=;")
            && detail.contains("; 1 the log no longer holds: no time")
            && detail.contains("; 2 closed turn(s) whose open was evicted name no ledger="),
        "{detail}"
    );
}

#[test]
fn a_log_that_cannot_be_read_places_nothing_and_says_so() {
    let mut host = logless_host();
    host.replies.insert(
        "log".to_string(),
        VecDeque::from(vec![err("unknown verb (try: help)"), ok("")]),
    );
    let l = read(&mut host, &opts(None));
    assert!(!l.aligned);
    assert_eq!(l.turns.len(), 3);
    assert!(l.turns.iter().all(|t| t.t.is_none()));
    let log = l
        .sources
        .iter()
        .find(|s| s.name == "log")
        .expect("a log source");
    assert!(
        !log.ok && log.detail.contains("no turn is placed"),
        "{}",
        log.detail
    );
    let mail = l.sources.iter().find(|s| s.name == "mail").expect("mail");
    assert!(
        mail.detail.contains("1 the log no longer holds: no time")
            && mail.detail.contains("0 post(s) to it in the log"),
        "{}",
        mail.detail
    );
    assert_eq!(l.mail.len(), 1);
    assert_eq!(l.mail[0].t, None);
}

/// The ledger reads the worker's `log class=turn` and this session's `log kinds=mail`, never
/// `timeline`, and places every turn by its log record.
#[test]
fn the_ledger_reads_the_log_and_no_clock() {
    let mut host = whole_host();
    let l = read(&mut host, &opts(None));
    for asked in ["@s-work log class=turn", "@self log kinds=mail"] {
        assert!(
            host.requests.iter().any(|r| r == asked),
            "{asked}: {:?}",
            host.requests
        );
    }
    assert!(
        !host.requests.iter().any(|r| r.ends_with(" timeline")),
        "{:?}",
        host.requests
    );
    assert!(l.aligned);
    assert!(l.turns.iter().all(|t| t.t.is_some()), "{:#?}", l.turns);
    let clock = l.sources.iter().find(|s| s.name == "clock").expect("row");
    assert!(
        clock.ok && !clock.detail.contains("deprecated"),
        "{}",
        clock.detail
    );
}

/// The ledger's three renders, byte for byte
/// (`fixtures/ledger-log-default.{txt,md,html}`): the journal's path reads `<journal>`, and
/// the page's fixed style and script are left out of the HTML's.
#[test]
fn the_ledger_renders_its_goldens() {
    let (dir, path) = journal_file("golden", &journal_lines());
    let l = read(&mut whole_host(), &opts(Some(path.clone())));
    let named = path.display().to_string();
    let shown = |format: Format| render(&l, format).replace(&named, "<journal>");
    let html = shown(Format::Html)
        .replace(super::super::ledger_html::STYLE, "<style>…</style>\n")
        .replace(super::super::ledger_html::SCRIPT, "…\n");
    for (got, want, what) in [
        (
            shown(Format::Text),
            include_str!("fixtures/ledger-log-default.txt"),
            "text",
        ),
        (
            shown(Format::Md),
            include_str!("fixtures/ledger-log-default.md"),
            "md",
        ),
        (
            html,
            include_str!("fixtures/ledger-log-default.html"),
            "html",
        ),
    ] {
        assert!(got == want, "the {what} render moved:\n{got}");
    }
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

/// When the worker's `log` cannot be read, the ledger places no turn, says so where the latency
/// would be, and prints journal times with no `~` in text, md and html.
#[test]
fn an_unread_log_guesses_no_time() {
    let (dir, path) = journal_file("log-unread", &journal_lines());
    let l = read(&mut logless_host(), &opts(Some(path.clone())));
    assert!(!l.aligned, "{:?}", l.sources);
    assert!(l.turns.iter().all(|t| t.t.is_none()));
    assert!(l.journal.iter().any(|r| r.line == "TIMEOUT"));
    let text = render(&l, Format::Text);
    assert!(
        text.contains("- (the session log could not be read)"),
        "{text}"
    );
    for (what, page) in [
        ("text", text.clone()),
        ("md", render(&l, Format::Md)),
        ("html", render(&l, Format::Html)),
    ] {
        assert!(!page.contains('~'), "the {what} render guesses:\n{page}");
    }
    let read_log = read(&mut whole_host(), &opts(Some(path.clone())));
    assert!(read_log.aligned);
    assert!(read_log.turns.iter().all(|t| t.t.is_some()));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

/// Each [`Word`] but `EVENT`, and each [`Noted`] word, marks its lines with a kind no other word,
/// `EVENT` phase, turn, reply or mail shares; each `EVENT` phase the loops print has its mark; an
/// older build's `other` line of a known word takes that word's mark; an unknown word, phase or
/// kind reads `unread`.
#[test]
fn every_journal_word_has_its_own_mark() {
    let mark = |line: &str| MarkKind::of_record(&JournalRecord::of_line(1, None, line, None));
    let not_a_words = [
        MarkKind::Turn,
        MarkKind::Reply,
        MarkKind::MailIn,
        MarkKind::MailOut,
        MarkKind::Unread,
    ];
    let mut events = Vec::new();
    for (phase, want) in [
        ("idle", MarkKind::Idle),
        ("turn", MarkKind::Idle),
        ("idle-no-report", MarkKind::Idle),
        ("resumed", MarkKind::Idle),
        ("question", MarkKind::Question),
        ("prompt", MarkKind::Prompt),
        ("limited", MarkKind::Limited),
        ("still-limited", MarkKind::Limited),
        ("survey", MarkKind::Survey),
        ("context", MarkKind::Context),
        ("compacted", MarkKind::Context),
        ("busy", MarkKind::Busy),
    ] {
        assert_eq!(
            mark(&format!("EVENT {phase} seq=1 x")),
            want,
            "EVENT {phase}"
        );
        events.push(want);
    }
    for phase in [
        aterm_phase::Phase::Busy,
        aterm_phase::Phase::Prompt,
        aterm_phase::Phase::Limited {
            message: String::new(),
            reset: None,
        },
        aterm_phase::Phase::Idle,
        aterm_phase::Phase::Question,
    ] {
        let m = mark(&format!("EVENT {} seq=1 x", phase.name()));
        assert!(events.contains(&m), "EVENT {} reads {m:?}", phase.name());
    }
    let mut owners: BTreeMap<&str, String> = BTreeMap::new();
    let printed = Word::ALL
        .into_iter()
        .filter(|w| *w != Word::Event)
        .map(|w| (format!("{w:?}"), w.printed()))
        .chain(
            Noted::ALL
                .into_iter()
                .map(|w| (format!("{w:?}"), w.printed().to_string())),
        );
    for (word, printed) in printed {
        for line in [format!("{printed} seq=1 x"), printed.clone()] {
            let m = mark(&line);
            assert!(
                !not_a_words.contains(&m) && !events.contains(&m),
                "{word} has no mark of its own: {m:?} ({line})"
            );
            match owners.get(m.name()) {
                Some(other) if *other != word => panic!("{word} shares {m:?} with {other}"),
                _ => owners.insert(m.name(), word.clone()),
            };
        }
    }
    for w in Noted::ALL {
        let r = JournalRecord::of_line(1, None, &format!("{} seq=1 x", w.printed()), None);
        assert_eq!(
            (r.kind.as_str(), MarkKind::of_record(&r)),
            ("other", MarkKind::Noted(w)),
            "{w:?} is kept as `other` and marked by its word"
        );
    }
    let mut seen: Vec<MarkKind> = owners
        .keys()
        .map(|name| {
            *MarkKind::ALL
                .iter()
                .find(|m| m.name() == *name)
                .unwrap_or_else(|| panic!("{name} is not in MarkKind::ALL"))
        })
        .collect();
    seen.extend(events);
    seen.extend(not_a_words);
    let mut names: Vec<&str> = MarkKind::ALL.iter().map(|m| m.name()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), MarkKind::ALL.len(), "two marks share a name");
    assert!(
        MarkKind::ALL.iter().all(|m| seen.contains(m)),
        "a mark nothing marks"
    );
    for line in [
        "PROBING seq=1 held",
        "EVENT zebra seq=1 x",
        "reset seq=1 rule=limit-reset@v1 x",
        "DONEX seq=1 x",
        "NOT seq=1 x",
    ] {
        assert_eq!(mark(line), MarkKind::Unread, "{line}");
    }
    let mut newer = JournalRecord::of_line(1, None, "HOLD seq=1 point x", None);
    newer.kind = "zebra".to_string();
    assert_eq!(
        MarkKind::of_record(&newer),
        MarkKind::Unread,
        "another build's word"
    );
    let mut older = JournalRecord::of_line(1, None, "HOLD seq=1 point x", None);
    older.kind = "other".to_string();
    assert_eq!(
        MarkKind::of_record(&older),
        MarkKind::Hold,
        "an older build kept HOLD as `other`"
    );
}

/// A test build's journal refuses a line whose word the ledger would read `unread`
/// (`assert_marked`), so every loop test holds its producers to [`Word`] and [`Noted`].
#[test]
fn a_loop_journals_only_what_the_ledger_reads() {
    for line in [
        "DONE seq=4 rule=finished@v1 the worker reported it",
        "NOT ESCALATED seq=2 the notice n1 stands: x",
        "KEPT attention for the loop that takes the session over",
        "HOLD-ENDED seq=3 a person, but nobody has touched the box",
        "EVENT idle seq=1 x",
        "TIMEOUT",
    ] {
        super::assert_marked(line);
        assert!(
            super::super::journal::Journal::off()
                .record(1, line, &mut Vec::new())
                .is_none(),
            "{line}"
        );
    }
    let refused = std::panic::catch_unwind(|| {
        super::super::journal::Journal::off().record(1, "PROBING seq=1 x", &mut Vec::new())
    });
    let why = refused.expect_err("an unread word is refused");
    let why = why
        .downcast_ref::<String>()
        .map(String::as_str)
        .unwrap_or_default();
    assert!(why.contains("`PROBING seq=1 x`"), "{why}");
}

/// The script colours every mark by its item's swatch class (`colour(it.cls)`), and each class
/// takes the colour its `.swatch` rule shows. The golden page leaves the script out, so only this
/// test reads it.
#[test]
fn the_page_colours_each_mark_as_its_legend_row_does() {
    use super::super::ledger_html::{SCRIPT, STYLE, swatch};
    let calls = SCRIPT.matches("colour(").count();
    let by_class = SCRIPT.matches("colour(it.cls)").count();
    assert!(
        by_class >= 2,
        "the turn's and the watcher's marks: {by_class}"
    );
    assert_eq!(
        by_class,
        calls - 1,
        "every mark is coloured by its class (the other call is the definition)"
    );
    let body = SCRIPT
        .split_once("function colour(cls) {")
        .and_then(|(_, rest)| rest.split_once("\n  }\n"))
        .map(|(body, _)| body)
        .expect("the script's colour function");
    let drawn = |cls: &str| -> String {
        let branch = format!("if (cls === \"{cls}\") {{ return \"");
        let rest = match body.split_once(&branch) {
            Some((_, rest)) => rest,
            None => body
                .rsplit_once("return \"")
                .map(|(_, rest)| rest)
                .expect("a default colour"),
        };
        rest.split_once('"')
            .map(|(colour, _)| colour.to_string())
            .expect("a quoted colour")
    };
    let shown = |cls: &str| -> String {
        let rule = format!(".swatch.{cls} {{ background: ");
        STYLE
            .split_once(&rule)
            .and_then(|(_, rest)| rest.split_once(';'))
            .map(|(colour, _)| colour.to_string())
            .unwrap_or_else(|| panic!("no swatch rule for {cls}"))
    };
    for m in MarkKind::ALL {
        if matches!(m, MarkKind::Reply | MarkKind::MailIn | MarkKind::MailOut) {
            continue;
        }
        let cls = swatch(m);
        assert_eq!(drawn(cls), shown(cls), "{m:?} is drawn in {cls}");
    }
}

/// The page's legend names each [`MarkKind`] once, with the `working` bar after the reply, each in
/// a swatch the style draws; an item's data says its swatch.
#[test]
fn every_mark_has_its_own_legend_row() {
    use super::super::ledger_html::{STYLE, legend, swatch};
    let (dir, path) = journal_file("legend", &journal_lines());
    let l = read(&mut whole_host(), &opts(Some(path.clone())));
    let html = render(&l, Format::Html);
    let rows = legend();
    assert_eq!(
        rows.len(),
        MarkKind::ALL.len() + 1,
        "one row per mark, and working"
    );
    for m in MarkKind::ALL {
        let row = format!(
            "<li data-kind=\"{}\"><span class=\"swatch {}\"></span>",
            m.name(),
            swatch(m)
        );
        assert_eq!(html.matches(&row).count(), 1, "{m:?}: {row}");
    }
    assert_eq!(
        rows.iter().position(|r| r.0 == "working"),
        rows.iter().position(|r| r.0 == "reply").map(|i| i + 1),
        "the working bar follows the reply"
    );
    for (_, cls, what) in &rows {
        assert!(
            STYLE.contains(&format!(".swatch.{cls} {{")),
            "no swatch rule for {cls}"
        );
        assert!(!what.is_empty());
    }
    for (kind, cls) in [
        ("approved", "event"),
        ("reconnect", "warn"),
        ("timeout", "bad"),
        ("turn", "turn"),
        ("mail-in", "mail"),
    ] {
        assert!(
            html.contains(&format!("\"kind\":\"{kind}\",\"cls\":\"{cls}\"")),
            "an item's data says its swatch: {kind}"
        );
    }
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

/// A turn carried from an earlier aterm is placed by its own log record, and its history row does
/// not call it timeless; a turn the log does not hold has no time.
#[test]
fn a_carried_turn_the_log_holds_is_placed_by_it() {
    let body = "turn 7 submitted=1 status=settled started_ms=5 dur_ms=3 seq=4 hash=0 \
                arch=7730:9 carried=1 text=old\n\
                turn 8 submitted=1 status=settled started_ms=9 dur_ms=3 seq=5 hash=0 \
                arch=7730:10 text=next\n";
    let history_detail = |l: &Ledger| {
        l.sources
            .iter()
            .find(|s| s.name == "history")
            .expect("row")
            .detail
            .clone()
    };
    let mut host = whole_host();
    host.replies
        .insert("history".to_string(), VecDeque::from(vec![ok(body)]));
    host.replies.insert(
        "log".to_string(),
        VecDeque::from(vec![
            ok(&format!(
                "5 open t1 turn par=r1 ledger=7 by=ctl src=screen conf=strong t={}\n",
                EPOCH + 500
            )),
            log_mail_reply(),
        ]),
    );
    let l = read(&mut host, &opts(None));
    assert_eq!(l.turns[0].t, Some(EPOCH + 500), "placed by its own record");
    assert_eq!(
        l.turns[1].t, None,
        "a turn the log does not hold has no time"
    );
    let detail = history_detail(&l);
    assert!(
        detail.contains("1 carried from an earlier aterm") && !detail.contains("no time"),
        "{detail}"
    );
}
