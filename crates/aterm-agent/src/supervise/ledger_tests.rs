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

fn timeline() -> CtlReply {
    ok(&format!(
        "event 1 t=0 kind=spawned state=spawning\n\
         event 2 t=5000 kind=post 8 to=@{WORKER} kind=task\n\
         event 3 t=5010 kind=post-landed 8 off=3\n\
         event 4 t=209000 kind=post 9 to=@{WORKER} kind=task\n\
         event 5 t=209500 kind=post 10 to=@s-other kind=note\n"
    ))
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

fn whole_host() -> Host {
    Host::default()
        .on("whoami", ok(&format!("OK {MANAGER} deadbeef owner\n")))
        .on("history", history())
        .on("offscreen", offscreen())
        .on("inbox", inbox())
        .on("timeline", timeline())
}

fn anchors(sid: &str) -> Result<ClockAnchor, String> {
    match sid {
        WORKER | MANAGER => Ok(ClockAnchor {
            pid: 66_439,
            epoch_ms: EPOCH,
            how: "its control socket's birth time".to_string(),
        }),
        other => Err(format!("no live instance hosts @{other}")),
    }
}

fn read(
    host: &mut Host,
    opts: &LedgerOpts,
    anchor: &mut dyn FnMut(&str) -> Result<ClockAnchor, String>,
) -> Ledger {
    let mut h = LedgerHost {
        now_ms: EPOCH + 300_000,
        tz_offset_s: 0,
        anchor,
    };
    gather(host, opts, &mut h)
}

fn opts(journal: Option<std::path::PathBuf>) -> LedgerOpts {
    LedgerOpts {
        worker: format!("@{WORKER}"),
        journal,
        since_ms: None,
        source: LedgerSource::Anchor,
    }
}

#[test]
fn the_ledger_joins_the_four_sources() {
    let (dir, path) = journal_file("whole", &journal_lines());
    let mut host = whole_host();
    let l = read(&mut host, &opts(Some(path.clone())), &mut anchors);
    assert_eq!(
        host.requests,
        [
            "whoami",
            "@s-work history",
            "@s-work offscreen tail=20000 max=20000 screen=1",
            "@self inbox --peek --meta",
            "@self timeline",
        ]
    );
    assert_eq!(l.worker, WORKER);
    assert_eq!(l.manager.as_deref(), Some(MANAGER));
    assert!(l.aligned);
    type Placed = (u64, Option<i64>, Result<usize, String>, Option<i64>);
    let turns: Vec<Placed> = l
        .turns
        .iter()
        .map(|t| (t.id, t.t, t.rows.clone(), t.stopped))
        .collect();
    assert_eq!(
        turns,
        [
            (1, Some(EPOCH + 10_000), Ok(3), Some(EPOCH + 20_000)),
            (2, Some(EPOCH + 100_000), Ok(2), Some(EPOCH + 150_000)),
            (3, Some(EPOCH + 200_000), Ok(4), Some(EPOCH + 210_000)),
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
                t: Some(EPOCH + 5_000),
                dir: Dir::Out,
                kind: "task".to_string(),
                len: None,
                trust: None,
                pending: false,
            },
            Mail {
                t: Some(EPOCH + 205_000),
                dir: Dir::In,
                kind: "report".to_string(),
                len: Some(2009),
                trust: Some("agent".to_string()),
                pending: false,
            },
            Mail {
                t: Some(EPOCH + 209_000),
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
        (3, 10_000 + 50_000 + 10_000)
    );
    assert_eq!(s.latencies, [80_000, 50_000]);
    assert_eq!(
        (s.median_ms(), s.max_ms(), s.waiting),
        (Some(65_000), Some(80_000), 1)
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
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

#[test]
fn the_md_form_is_tables() {
    let (dir, path) = journal_file("md", &journal_lines());
    let mut host = whole_host();
    let mut l = read(&mut host, &opts(Some(path.clone())), &mut anchors);
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
    let mut l = read(&mut host, &opts(Some(path.clone())), &mut anchors);
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
    let l = read(&mut host, &opts(Some(path.clone())), &mut anchors);
    let misses: Vec<&str> = l.sources.iter().filter(|s| !s.ok).map(|s| s.name).collect();
    assert_eq!(misses, ["history", "mail"]);
    assert!(l.turns.is_empty() && l.mail.is_empty());
    assert_eq!(l.journal.len(), 11);
    let mut host = whole_host();
    let l = read(&mut host, &opts(None), &mut anchors);
    assert!(!l.journaled && !l.journal_unreadable);
    assert_eq!(l.turns.len(), 3);
    assert!(l.turns.iter().all(|t| t.stopped.is_none()));
    let l = read(
        &mut whole_host(),
        &opts(Some(dir.join("nothing-here.jsonl"))),
        &mut anchors,
    );
    assert!(!l.journaled && l.journal_unreadable);
    let journal = l.sources.iter().find(|s| s.name == "journal").expect("row");
    assert!(!journal.ok, "{}", journal.detail);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

#[test]
fn without_a_clock_no_working_time_is_claimed() {
    let (dir, path) = journal_file("clockless-spans", &journal_lines());
    let mut host = whole_host();
    let l = read(&mut host, &opts(Some(path.clone())), &mut |sid| {
        Err(format!("no live instance hosts @{sid}"))
    });
    assert!(!l.aligned);
    assert!(
        l.turns.iter().all(|t| t.stopped.is_none()),
        "a stop read off ANOTHER clock is not this turn's stop"
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
    let clock = l.sources.iter().find(|s| s.name == "clock").expect("row");
    assert!(!clock.ok);
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
    let l = read(&mut host, &opts(None), &mut anchors);
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
    let l = read(&mut host, &opts(None), &mut anchors);
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
    let l = read(&mut host, &opts(None), &mut anchors);
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
        &mut anchors,
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
    let mut host = whole_host();
    host.replies
        .insert("history".to_string(), VecDeque::from(vec![ok(body)]));
    let l = read(&mut host, &opts(None), &mut anchors);
    assert_eq!(l.turns[0].t, None, "a carried turn is on another clock");
    assert!(l.turns[1].t.is_some());
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
        read(&mut host, &opts(None), &mut anchors)
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
    let l = read(&mut host, &opts(None), &mut anchors);
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
    let l = read(&mut host, &opts(None), &mut anchors);
    assert_eq!(l.turns[0].swallowed, [2, 3]);
    assert!(l.turns[1].rows.is_err() && l.turns[2].rows.is_err());
}

const ANCHOR_ERROR_MS: i64 = 1_000;
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

fn log_host(restated: bool) -> Host {
    whole_host()
        .on("log", log_turns_reply(restated))
        .on("log", log_mail_reply())
}

fn log_opts() -> LedgerOpts {
    LedgerOpts {
        source: LedgerSource::Log,
        ..opts(None)
    }
}

fn read_log(host: &mut Host) -> Ledger {
    read(host, &log_opts(), &mut |sid| {
        panic!("--source log asked for @{sid}'s clock anchor")
    })
}

fn near(a: Option<i64>, b: Option<i64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() <= ANCHOR_ERROR_MS,
        (None, None) => true,
        _ => false,
    }
}

type Untimed = (
    u64,
    String,
    bool,
    u64,
    bool,
    String,
    Result<usize, String>,
    Vec<u64>,
);

fn untimed(l: &Ledger) -> Vec<Untimed> {
    l.turns
        .iter()
        .map(|t| {
            (
                t.id,
                t.status.clone(),
                t.submitted,
                t.dur_ms,
                t.carried,
                t.text.clone(),
                t.rows.clone(),
                t.swallowed.clone(),
            )
        })
        .collect()
}

#[test]
fn log_and_anchor_paths_agree_modulo_times() {
    let anchor = read(&mut whole_host(), &opts(None), &mut anchors);
    let mut host = log_host(true);
    let log = read_log(&mut host);
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
    assert!(log.aligned && anchor.aligned);
    assert_eq!(untimed(&log), untimed(&anchor));
    for (l, a) in log.turns.iter().zip(&anchor.turns) {
        assert!(
            l.t.is_some() && near(l.t, a.t),
            "turn {}: {:?} {:?}",
            l.id,
            l.t,
            a.t
        );
    }
    assert_eq!(log.mail.len(), anchor.mail.len());
    for (l, a) in log.mail.iter().zip(&anchor.mail) {
        assert!(near(l.t, a.t), "{l:?} {a:?}");
        assert_eq!(
            (l.dir, &l.kind, l.len, &l.trust, l.pending),
            (a.dir, &a.kind, a.len, &a.trust, a.pending)
        );
    }
    let src = |l: &Ledger, name: &str| {
        l.sources
            .iter()
            .find(|s| s.name == name)
            .map(|s| (s.ok, s.detail.clone()))
    };
    let (ok, detail) = src(&log, "log").expect("a log source");
    assert!(ok, "{detail}");
    assert!(
        detail.starts_with(
            "@s-work log class=turn: 3 of 3 turn(s) placed by ledger= (1 by its close: the \
             open was evicted)"
        ) && detail.contains("the ring evicted 9 record(s)"),
        "{detail}"
    );
    assert!(
        detail.contains(
            "; 1 closed turn(s) whose open was evicted name no ledger= (no `turn` exchange \
             opened them, or an older aterm closed them): joined to nothing"
        ) && !detail.contains("ledger= is gone")
            && !detail.contains("no longer holds"),
        "{detail}"
    );
    let what = |l: &Ledger, id: u64| {
        l.items()
            .into_iter()
            .find(|i| i.what.starts_with(&format!("turn {id} ")))
            .map(|i| i.what)
    };
    for id in 1..=3 {
        let (l, a) = (what(&log, id), what(&anchor, id));
        assert!(
            l.as_deref()
                .is_some_and(|w| w.starts_with(&format!("turn {id} began: ")))
                && a.as_deref()
                    .is_some_and(|w| w.starts_with(&format!("turn {id} sent: "))),
            "{l:?} {a:?}"
        );
    }
    assert!(src(&anchor, "log").is_none(), "the old path reads no log");
    let (ok, clock) = src(&log, "clock").expect("a clock source");
    assert!(ok && clock.contains("no clock anchor"), "{clock}");
    let (s, a) = (log.summary(), anchor.summary());
    assert_eq!(
        (s.turns, s.settled, s.timeouts, s.mail_in, s.mail_out),
        (a.turns, a.settled, a.timeouts, a.mail_in, a.mail_out)
    );
}

#[test]
fn a_close_without_its_ledger_loses_the_join() {
    let anchor = read(&mut whole_host(), &opts(None), &mut anchors);
    let log = read_log(&mut log_host(false));
    assert_eq!(untimed(&log), untimed(&anchor), "the rows still read");
    assert_eq!(log.turns[0].t, None, "turn 1 is not joined by time");
    assert!(anchor.turns[0].t.is_some());
    assert!(near(log.turns[1].t, anchor.turns[1].t));
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
    let mut host = whole_host()
        .on("log", err("unknown verb (try: help)"))
        .on("log", ok(""));
    let l = read_log(&mut host);
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
