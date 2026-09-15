// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ledger`, over a scripted host and a real journal file on disk: what each
//! source contributes, the clock, the latency numbers, the three formats, and
//! what it still prints when a source is missing.

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

/// The worker's process clock's zero, and the times in it.
const EPOCH: i64 = 1_789_000_000_000;
const WORKER: &str = "s-work";
const MANAGER: &str = "s-mgr";

/// The three turns the manager typed, as `history` prints them.
fn history() -> CtlReply {
    let pct = |s: &str| -> String {
        s.bytes()
            .map(|b| {
                if b.is_ascii_graphic() && b != b'%' {
                    char::from(b).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect()
    };
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
            pct(text)
        ));
    }
    ok(&out)
}

/// The screen the worker is on: the third turn's row, its reply, a done row.
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

/// One `offscreen … screen=1` reply, as `aterm ctl` prints it: the rows on
/// stdout, the header on stderr.
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

/// The same read with `archived` as the rows the archive holds.
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

/// This session's inbox: one message from the worker, one from someone else,
/// and one post to the worker that has not landed.
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

/// This session's timeline: one landed post to the worker, one still queued,
/// and a post to another session.
fn timeline() -> CtlReply {
    ok(&format!(
        "event 1 t=0 kind=spawned state=spawning\n\
         event 2 t=5000 kind=post 8 to=@{WORKER} kind=task\n\
         event 3 t=5010 kind=post-landed 8 off=3\n\
         event 4 t=209000 kind=post 9 to=@{WORKER} kind=task\n\
         event 5 t=209500 kind=post 10 to=@s-other kind=note\n"
    ))
}

/// The journal the watcher wrote, on disk.
fn journal_file(tag: &str, lines: &[String]) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("aterm-ledger-{tag}-{}", std::process::id()));
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

/// The watcher's lines: one per kind, and two that are not this worker's.
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
    // Another session's watch wrote into the same file, and a line was cut
    // short by the crash that ended it.
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

/// A host with every source scripted.
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
    }
}

/// The whole path: every source read, every request made once, the turns
/// placed on the wall clock with their reply sizes, the journal filtered to
/// this worker, the mail in both directions, and the summary's numbers.
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
    // The turns: placed, sized, and each with the EVENT that ended it.
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
    // The journal: this worker's lines only, the other session's and the cut
    // one left out, and both counted in SOURCES.
    assert_eq!(l.journal.len(), 11);
    let journal = l
        .sources
        .iter()
        .find(|s| s.name == "journal")
        .expect("a journal source");
    assert!(
        journal.ok
            && journal.detail.contains("11 line(s) about @s-work")
            && journal.detail.contains("1 about other sessions skipped")
            && journal.detail.contains("1 unreadable line(s) skipped"),
        "{}",
        journal.detail
    );
    // The mail: one in from the worker, two posts out (one not landed).
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
    // The numbers, including the latency from each EVENT idle/question to the
    // next turn: 80 s and 50 s (the last idle has no turn after it).
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
    assert_eq!((s.complete, s.incomplete), (1, 1));
    assert_eq!((s.timeouts_seen, s.exits), (1, 0));
    // Every item is in time order, one lane each.
    let items = l.items();
    let times: Vec<Option<i64>> = items.iter().map(|i| i.t).collect();
    let mut sorted = times.clone();
    sorted.sort();
    assert_eq!(times, sorted);
    assert_eq!(items.len(), 3 * 2 + 11 + 3);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

/// The text form: the header, the sources, the summary and the timeline's
/// rows — the turn's first 100 characters, the EVENT lines verbatim, the
/// reply sizes, and the latency on the EVENT that was answered.
#[test]
fn the_text_form_reads_as_one_timeline() {
    let (dir, path) = journal_file("text", &journal_lines());
    let mut host = whole_host();
    let l = read(&mut host, &opts(Some(path.clone())), &mut anchors);
    let text = render_text(&l);
    for want in [
        "aterm drive ledger — worker @s-work · manager @s-mgr · 2026-09-10 00:31:40 · times UTC+00:00",
        "✓ history    3 turn(s) from @s-work",
        "  turns driven      3 (2 settled, 1 timeout)",
        "  manager latency   median 1m05s · max 1m20s over 2 EVENT idle/question · 1 still waiting",
        "  approvals         1",
        "  mail              1 in · 2 out",
        "  reports           1 complete · 1 incomplete",
        "turn 1 sent: Go on the amended lever",
        "settled in 1.8s",
        "turn 1 reply: 3 rows",
        "working 10s",
        "EVENT idle seq=5 complete=1 rows=689 ⏺ Done with the lever.",
        "answered in 1m20s",
        "mail out: task to the worker",
        "mail in: report from the worker, 2009 B, trust=agent",
        "mail out: task to the worker, not landed yet",
        "timeout after 6.0s",
        "no turn since",
    ] {
        assert!(
            text.contains(want),
            "the text form is missing {want:?}:\n{text}"
        );
    }
    // The lanes, in the order the rows came.
    let lanes: Vec<&str> = text
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .filter(|w| ["manager", "worker", "watcher", "fabric"].contains(w))
        .collect();
    assert_eq!(lanes[..4], ["fabric", "manager", "worker", "watcher"]);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

/// The markdown form: the same content as tables, with `|` escaped so a
/// worker's own pipe cannot break a row.
#[test]
fn the_md_form_is_tables() {
    let (dir, path) = journal_file("md", &journal_lines());
    let mut host = whole_host();
    let mut l = read(&mut host, &opts(Some(path.clone())), &mut anchors);
    l.turns[0].text = "a | b".to_string();
    let md = render_md(&l);
    assert!(md.starts_with("# aterm drive ledger\n"), "{md}");
    for want in [
        "## Summary\n",
        "| turns driven | 3 (2 settled, 1 timeout) |",
        "## Sources\n",
        "| history | yes |",
        "## Timeline\n",
        "| time | lane | what | duration/latency |",
        "turn 1 sent: a \\| b",
    ] {
        assert!(md.contains(want), "the md form is missing {want:?}:\n{md}");
    }
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

/// The HTML form is ONE file: no URL it would fetch, a policy that forbids
/// one, every tag closed, and everything a worker said escaped — into the
/// page as text and into the embedded data as `\u` escapes, so neither a
/// `</script>` in a transcript nor a tag in a command can close one.
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
    // The hostile text survives as text, and nowhere as markup.
    assert!(
        html.contains("&lt;/script&gt;&lt;img src=http://evil/x&gt; &amp; &lt;b&gt;bold&lt;/b&gt;"),
        "the worker's own tags are text, not markup"
    );
    assert!(
        html.contains("\\u003c/script\\u003e"),
        "the data is escaped"
    );
    assert_eq!(html.matches("</script>").count(), 2, "one close per script");
    // The three lanes and the mail lane are all drawn and all in the table.
    for want in ["\"manager\"", "\"watcher\"", "\"worker\"", "\"fabric\""] {
        assert!(html.contains(want), "the html is missing lane {want}");
    }
    assert!(html.contains("<h2>Swimlanes</h2>") && html.contains("<h2>Timeline</h2>"));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

/// Every tag in `html` closes, in order, outside `<script>`/`<style>`; void
/// elements close themselves. (A small check, not a browser: it catches an
/// unbalanced or unescaped tag, which is what a generator gets wrong.)
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
        // A script's or style's body is not markup: skip to its close.
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

/// A missing source is NAMED and the rest still prints: a host that knows
/// none of the verbs, with a journal, gives the watcher's lane and four
/// misses.
#[test]
fn a_missing_source_is_named_and_the_rest_prints() {
    let (dir, path) = journal_file("missing", &journal_lines());
    let mut host = Host::default();
    let l = read(&mut host, &opts(Some(path.clone())), &mut anchors);
    let misses: Vec<&str> = l.sources.iter().filter(|s| !s.ok).map(|s| s.name).collect();
    assert_eq!(misses, ["history", "mail"]);
    assert!(l.turns.is_empty() && l.mail.is_empty());
    assert_eq!(l.journal.len(), 11);
    let text = render_text(&l);
    assert!(text.contains("✗ history"), "{text}");
    assert!(text.contains("has no `history`"), "{text}");
    assert!(text.contains("EVENT idle seq=5"), "{text}");
    assert!(
        text.contains("manager latency   median 1m05s") || text.contains("manager latency   -"),
        "{text}"
    );
    // No journal: the lane and its numbers say so, and the turns still print.
    let mut host = whole_host();
    let l = read(&mut host, &opts(None), &mut anchors);
    let text = render_text(&l);
    assert!(
        text.contains("  approvals         - (no journal)"),
        "{text}"
    );
    assert!(
        text.contains("turn 1 sent: Go on the amended lever"),
        "{text}"
    );
    assert!(l.turns.iter().all(|t| t.stopped.is_none()));
    // An unreadable journal is a miss, not an error.
    let l = read(
        &mut whole_host(),
        &opts(Some(dir.join("nothing-here.jsonl"))),
        &mut anchors,
    );
    let journal = l.sources.iter().find(|s| s.name == "journal").expect("row");
    assert!(
        !journal.ok && journal.detail.starts_with("unreadable: "),
        "{}",
        journal.detail
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

/// Without a clock the rows still read: the times are marked `~`, no latency
/// is claimed, and the clock source says why.
#[test]
fn without_a_clock_the_times_are_marked_and_no_latency_is_claimed() {
    let (dir, path) = journal_file("clockless", &journal_lines());
    let mut host = whole_host();
    let l = read(&mut host, &opts(Some(path.clone())), &mut |sid| {
        Err(format!("no live instance hosts @{sid}"))
    });
    assert!(!l.aligned);
    let clock = l.sources.iter().find(|s| s.name == "clock").expect("row");
    assert!(
        !clock.ok && clock.detail.contains("not placed on the wall clock"),
        "{}",
        clock.detail
    );
    let s = l.summary();
    assert!(s.latencies.is_empty() && s.waiting == 0);
    let text = render_text(&l);
    assert!(
        text.contains("manager latency   - (the clock could not be placed)"),
        "{text}"
    );
    assert!(text.contains(" ~"), "the times are marked:\n{text}");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

/// **WITHOUT A CLOCK, NO WORKING TIME IS CLAIMED.**
///
/// `turn.t` is a PROCESS-clock stamp placed on an epoch; unanchored that epoch
/// is the fallback (the newest turn read taken as now) while a journal
/// record's `t` is the wall clock the watcher wrote. Before the fix
/// `turn.stopped` was set from the journal unconditionally and `working_ms +=
/// stopped - t` subtracted the two, so ONE render said both:
///
/// ```text
///   worker busy       9.5s in the turn verbs (sum of dur_ms) · 50s working over 2 turn(s) …
///   manager latency   - (the clock could not be placed)
///  ~00:28:30   worker   turn 1 reply: 3 rows        working 40s
/// ```
///
/// while the help, the CHANGELOG and the skill all say what cannot be placed
/// is marked `~` and claims no latency.
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
    let text = render_text(&l);
    assert!(
        text.contains("manager latency   - (the clock could not be placed)"),
        "{text}"
    );
    assert!(
        !text.contains("working"),
        "the render claims a working span it could not measure:\n{text}"
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

/// **A `--journal` THAT COULD NOT BE READ IS NOT A JOURNAL OF ZERO EVENTS.**
///
/// `journaled` was `opts.journal.is_some()` — the file was GIVEN, not read —
/// so an unreadable path printed the watcher's whole lane as measured zeroes
/// two lines under the `✗ journal` that said it could not be opened:
///
/// ```text
///   ✗ journal    unreadable: /nonexistent-dir/journal.jsonl: No such file or directory (os error 2)
///   manager latency   - (no EVENT idle/question in the journal)
///   approvals         0 / dismissals 0 / reconnects 0 / reports 0 complete · 0 incomplete
/// ```
#[test]
fn an_unreadable_journal_is_not_counted_as_zero() {
    let mut host = whole_host();
    let missing = std::path::PathBuf::from("/nonexistent-dir/journal.jsonl");
    let l = read(&mut host, &opts(Some(missing)), &mut anchors);
    assert!(!l.journaled, "nothing was read");
    assert!(l.journal_unreadable, "and one was asked for");
    let src = l.sources.iter().find(|s| s.name == "journal").expect("row");
    assert!(!src.ok && src.detail.starts_with("unreadable: "), "{src:?}");

    let text = render_text(&l);
    for lied in [
        "- (no EVENT idle/question in the journal)",
        "approvals         0",
        "reconnects        0",
    ] {
        assert!(
            !text.contains(lied),
            "the journal was never read, so {lied:?} is not something it knows:\n{text}"
        );
    }
    assert!(
        text.contains("manager latency   - (the journal could not be read)"),
        "{text}"
    );
    assert!(
        text.contains("approvals         - (the journal could not be read)"),
        "{text}"
    );

    // With NO journal asked for, the older wording still stands.
    let mut host = whole_host();
    let l = read(&mut host, &opts(None), &mut anchors);
    assert!(!l.journaled && !l.journal_unreadable);
    assert!(
        render_text(&l).contains("manager latency   - (no journal)"),
        "{}",
        render_text(&l)
    );
}

/// A turn whose rows the archive no longer holds, and one from another
/// archive, say so instead of claiming a size.
#[test]
fn a_reply_the_archive_lost_says_so() {
    let mut host = whole_host();
    // The archive starts after the first two turns' marks, and turn 3's row
    // is not in it either (the read is the screen alone).
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
    // A host with no `offscreen` at all: the same honest answer.
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
    assert!(
        !src.ok && src.detail.contains("reply sizes not known"),
        "{}",
        src.detail
    );
    let text = render_text(&l);
    assert!(
        text.contains("turn 1 reply: size not known (no archive read)"),
        "{text}"
    );
}

/// `--since` drops what came before it, from every source.
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
    let text = render_text(&l);
    assert!(text.contains("since 2026-09-10 00:29:20"), "{text}");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

/// `--since` takes Unix milliseconds or a time word: a date (local midnight),
/// a date and time (local, or the zone it names). Anything else is refused
/// rather than read as zero.
#[test]
fn since_takes_ms_or_a_time_word() {
    // 2026-09-14T00:00:00Z
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

/// The clock words: a span, a stamp in the local zone, the zone itself.
#[test]
fn the_clock_prints_what_a_human_reads() {
    assert_eq!(span(0), "0s");
    assert_eq!(span(59_999), "59s");
    assert_eq!(span(65_000), "1m05s");
    assert_eq!(span(3_600_000 + 125_000), "1h02m");
    assert_eq!(stamp(1_789_344_000_000, 0, true), "2026-09-14 00:00:00");
    assert_eq!(stamp(1_789_344_000_000, -7 * 3600, false), "17:00:00");
    assert_eq!(zone(-7 * 3600), "UTC-07:00");
    assert_eq!(zone(5 * 3600 + 1800), "UTC+05:30");
    assert_eq!(zone(0), "UTC+00:00");
}

/// A `history` record's every field, `carried=1` and a pct-encoded text
/// included; a turn a self-update carried has no time here.
#[test]
fn a_history_record_is_read_whole() {
    let body = "OK 2\n\
        turn 7 submitted=0 status=timeout started_ms=1197729 dur_ms=1761 seq=32979 \
        hash=d06718987e6891ec arch=1789083498272557936:185 carried=1 text=Manager%20here.%20Go\n\
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
    let src = l.sources.iter().find(|s| s.name == "history").expect("row");
    assert!(
        src.detail.contains("1 carried from an earlier aterm"),
        "{}",
        src.detail
    );
}

/// The report views are the blocks module's, reached through `ReportOpts`:
/// the default renders exactly as before, and a view adds `view=` and
/// `kept=` to the same header.
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

/// A turn whose own `❯` row is nowhere in the rows read — it was queued while
/// the worker was busy, or a restart took the row — has its reply inside the
/// previous turn's count, and the row that carries it says so rather than
/// claiming the count is one reply.
#[test]
fn a_turn_whose_row_is_missing_is_named_in_the_span_that_holds_it() {
    let mut host = whole_host();
    // The middle turn's row never appeared.
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
    assert_eq!(l.turns[1].rows, Err("its ❯ row was not found".to_string()));
    assert!(l.turns[2].rows.is_ok() && l.turns[2].swallowed.is_empty());
    let text = render_text(&l);
    assert!(
        text.contains("turn 1 reply: 4 rows (turn 2's too: its ❯ row was not found)"),
        "{text}"
    );
    // And when NO later turn's row is found, every one after it is named.
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
