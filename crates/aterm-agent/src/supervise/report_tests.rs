// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `Session::report` over a scripted [`Ctl`] host.

use std::collections::{BTreeMap, VecDeque};

use super::super::blocks::View;
use super::super::phase::transcript_end;
use super::super::prompt::fixtures::{composer, rows};
use super::*;

/// Scripted [`Ctl`] host that records requests and returns queued replies per verb or request.
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
        let words = &args[usize::from(args.first().is_some_and(|a| a.starts_with('@')))..];
        let verb = words.first().copied().unwrap_or("");
        let request = words.join(" ");
        let key = if self.replies.contains_key(&request) {
            request.as_str()
        } else {
            verb
        };
        if let Some(reply) = self.replies.get_mut(key).and_then(VecDeque::pop_front) {
            return Ok(reply);
        }
        Err(format!("unscripted request: {}", args.join(" ")))
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

/// `aterm-ctl: server closed the connection without responding`: not served.
fn closed() -> CtlReply {
    CtlReply {
        code: 1,
        stdout: String::new(),
        stderr: "aterm-ctl: server closed the connection without responding\n".to_string(),
    }
}

/// `history` as `aterm ctl` prints it: the records on stdout.
fn history(turns: &[(u64, bool, &str, &str)]) -> CtlReply {
    let mut out = String::new();
    for (id, submitted, arch, text) in turns {
        out.push_str(&format!(
            "turn {id} submitted={} status=settled started_ms=1 dur_ms=2 seq=3 \
             hash=0000000000000000 arch={arch} text={}\n",
            u8::from(*submitted),
            aterm_codec::pct::encode(text)
        ));
    }
    ok(&out)
}

/// `status` naming the session's foreground `program`.
fn status(program: &str) -> CtlReply {
    ok(&format!("OK sid=1 program={program} agent=-\n"))
}

/// An empty ledger, as `aterm ctl` prints it (the count on stderr).
fn no_history() -> CtlReply {
    CtlReply {
        code: 0,
        stdout: String::new(),
        stderr: "aterm-ctl: OK 0 (history: no results)\n".to_string(),
    }
}

/// One `offscreen … screen=1` read, before it is framed.
#[derive(Clone)]
struct Read {
    archived: Vec<String>,
    screen: Vec<String>,
    first: u64,
    last: u64,
    lost: u64,
    breaks: u64,
    back: usize,
    /// Where the `back` re-shown rows sit in the archive (default: the page's
    /// last `back` rows) and under how many pinned rows.
    back_at: Option<u64>,
    pin: usize,
    origin: u64,
    alt: bool,
    more: bool,
    /// The screen half caught the program's DEC 2026 frame unfinished (`frame=open`).
    open: bool,
}

impl Read {
    /// Rows `first..` archived, and the screen.
    fn new(first: u64, archived: &[&str], screen: Vec<String>) -> Self {
        let n = u64::try_from(archived.len()).expect("fits");
        Self {
            archived: rows(archived),
            screen,
            first,
            last: (first + n).saturating_sub(1),
            lost: 0,
            breaks: 0,
            back: 0,
            back_at: None,
            pin: 0,
            origin: 7730,
            alt: true,
            more: false,
            open: false,
        }
    }

    fn header(&self) -> String {
        let mut h = format!(
            "OK {} first={} last={} lost={} breaks={} back={}",
            self.archived.len() + self.screen.len(),
            self.first,
            self.last,
            self.lost,
            self.breaks,
            self.back,
        );
        if self.back > 0 {
            let at = self.back_at.unwrap_or(self.last + 1 - self.back as u64);
            h.push_str(&format!(" back_at={at} pin={}", self.pin));
        }
        h.push_str(&format!(
            " epoch=2 origin={} alt={} seq=900",
            self.origin,
            u8::from(self.alt)
        ));
        if self.more {
            h.push_str(" more=1");
        }
        if self.open {
            h.push_str(" frame=open");
        }
        h.push_str(&format!(" screen_rows={}", self.screen.len()));
        h
    }

    fn body(&self) -> String {
        self.archived
            .iter()
            .chain(&self.screen)
            .map(|r| format!("{r}\n"))
            .collect()
    }

    /// As `aterm ctl` prints it: the rows on stdout, the header on stderr.
    fn ctl(&self) -> CtlReply {
        CtlReply {
            code: 0,
            stdout: self.body(),
            stderr: format!("aterm-ctl: {}\n", self.header()),
        }
    }

    /// As a raw client sees it: the header, then the rows.
    fn raw(&self) -> CtlReply {
        ok(&format!("{}\n{}", self.header(), self.body()))
    }
}

/// The one-row read `report` places a gap with.
fn recheck(breaks: u64) -> CtlReply {
    CtlReply {
        code: 0,
        stdout: "row\n".to_string(),
        stderr: format!(
            "aterm-ctl: OK 1 first=104 last=110 lost=0 breaks={breaks} back=0 epoch=2 \
             origin=7730 alt=1 seq=901\n"
        ),
    }
}

/// Build a Claude Code screen from `transcript` rows, `live` status rows, and a composer frame.
fn claude(transcript: &[&str], live: &[&str]) -> Vec<String> {
    let mut r = rows(transcript);
    r.extend(rows(live));
    r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
    r
}

fn fixture(text: &str) -> Vec<String> {
    let mut r: Vec<String> = text.lines().map(str::to_string).collect();
    if r.first().is_some_and(|l| l.starts_with("== ")) {
        r.remove(0);
    }
    if r.last().is_some_and(|l| l.starts_with("exit=")) {
        r.pop();
    }
    r
}

fn run(host: &mut Host, opts: ReportOpts) -> Report {
    Session::new(host, Some("@s-1".to_string()))
        .report(&opts)
        .expect("report")
}

const LEVER: &str = "Go on the amended lever, with one correction: the ghost-key \
                     site comment says that entry is not safe to replay.";

#[test]
fn a_turn_that_scrolled_off_is_joined_back_from_its_user_row() {
    let screen = claude(
        &[
            "",
            "⏺ Reading the ghost-key site first.",
            "  ⎿  Read 40 lines",
            "",
            "  │ site      │ status │",
            "  ├───────────┼────────┤",
            "  │ evict.rs  │ unsafe │",
            "",
            "  ☐ Write the replay checker",
            "                        let replay = entry.clone();",
            "",
            "⏺ Waiting for your go on the checker?",
            "",
        ],
        &["✻ Cooked for 4s · done 2:41 PM", ""],
    );
    let mut read = Read::new(
        101,
        &[
            "⏺ The earlier turn's last words.",
            "✻ Churned for 9s · done 2:30 PM",
            "",
            "❯ Go on the amended lever, with one",
            "  correction: the ghost-key site comment says that entry is not",
            "  safe to replay.",
            "",
            "⏺ Reading the ghost-key site first.",
        ],
        screen,
    );
    read.back = 2;
    let mut host = Host::default()
        .on(
            "history",
            history(&[
                (41, true, "7730:60", "earlier"),
                (42, true, "7730:100", LEVER),
            ]),
        )
        .on("offscreen", read.ctl());
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(
        host.requests,
        [
            "@s-1 history 8",
            "@s-1 offscreen since=7730:100 max=8000 screen=1"
        ]
    );
    assert_eq!(
        report.header(),
        "report complete=1 marker=ledger turn=42 rows=17 archived=5 screen=12 last=7730:108"
    );
    assert_eq!(
        report.rows,
        rows(&[
            "❯ Go on the amended lever, with one",
            "  correction: the ghost-key site comment says that entry is not",
            "  safe to replay.",
            "",
            "⏺ Reading the ghost-key site first.",
            "  ⎿  Read 40 lines",
            "",
            "  │ site      │ status │",
            "  ├───────────┼────────┤",
            "  │ evict.rs  │ unsafe │",
            "",
            "  ☐ Write the replay checker",
            "                        let replay = entry.clone();",
            "",
            "⏺ Waiting for your go on the checker?",
            "",
            "✻ Cooked for 4s · done 2:41 PM",
        ])
    );
    assert_eq!(
        report.render(),
        format!("{}\n--\n{}\n", report.header(), report.rows.join("\n"))
    );
    let mut host = Host::default()
        .on("history", history(&[(42, true, "7730:100", LEVER)]))
        .on("offscreen", read.raw());
    assert_eq!(run(&mut host, ReportOpts::default()), report);
}

#[test]
fn a_wrapped_user_block_on_the_screen_is_the_start() {
    let screen = fixture(super::super::prompt::fixtures::WAIT_BG2);
    assert!(screen[transcript_end(&screen)].starts_with("✶ Deliberating"));
    let text = "Go on the amended lever, with one correction: the ghost-key site comment \
                says that entry is not safe to replay, so that gate has an auditing reason; \
                do not bypass it, either give it a sound replay the checker accepts or leave \
                it and say so.";
    let mut host = Host::default()
        .on("history", history(&[(9, true, "7730:0", text)]))
        .on("offscreen", Read::new(1, &[], screen).ctl());
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(
        report.header(),
        "report complete=1 marker=ledger turn=9 rows=7 archived=0 screen=7 last=7730:0"
    );
    assert!(report.rows[0].starts_with("❯ Go on the amended lever"));
    assert!(report.rows[6].starts_with("  Report after each site"));
}

#[test]
fn a_report_reads_past_the_harness_turns_to_the_managers() {
    let screen = fixture(super::super::prompt::fixtures::WAIT_BG2);
    let text = "Go on the amended lever, with one correction: the ghost-key site comment \
                says that entry is not safe to replay.";
    let ledger = |harness: std::ops::Range<u64>| {
        let mut body = history(&[(9, true, "7730:0", text)]).stdout;
        for id in harness {
            body.push_str(&format!(
                "turn {id} submitted=1 status=pressed started_ms=1 dur_ms=2 seq=3 \
                 hash=0000000000000000 by=harness arch=7730:0 text=keep%20going\n"
            ));
        }
        body
    };
    let newest = |body: &str, n: usize| {
        let rows: Vec<&str> = body.lines().collect();
        ok(&rows[rows.len().saturating_sub(n)..]
            .iter()
            .map(|r| format!("{r}\n"))
            .collect::<String>())
    };
    let whole = ledger(10..20);
    let mut host = Host::default()
        .on("history", newest(&whole, 8))
        .on("history", ok(&whole))
        .on("offscreen", Read::new(1, &[], screen.clone()).ctl());
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(
        host.requests,
        [
            "@s-1 history 8",
            "@s-1 history",
            "@s-1 offscreen since=7730:0 max=8000 screen=1"
        ]
    );
    assert!(
        report
            .header()
            .starts_with("report complete=1 marker=ledger turn=9 "),
        "{}",
        report.header()
    );

    let near = ledger(10..17);
    let mut host = Host::default()
        .on("history", newest(&near, 8))
        .on("offscreen", Read::new(1, &[], screen).ctl());
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(
        host.requests,
        [
            "@s-1 history 8",
            "@s-1 offscreen since=7730:0 max=8000 screen=1"
        ]
    );
    assert!(
        report
            .header()
            .starts_with("report complete=1 marker=ledger turn=9 "),
        "{}",
        report.header()
    );
}

#[test]
fn an_offscreen_read_of_an_open_frame_is_read_again_whole() {
    let whole = claude(
        &[
            "❯ first message",
            "⏺ answer one",
            "❯ second message",
            "⏺ working on it",
            "",
        ],
        &["✶ Deliberating… (4s)"],
    );
    let half: Vec<String> = whole[..3].to_vec();
    let mut open = Read::new(1, &[], half);
    open.open = true;
    let mut host = Host::default()
        .on("history", no_history())
        .on("offscreen", open.ctl())
        .on("await", ok("OK frame 901\n"))
        .on("offscreen", Read::new(1, &[], whole.clone()).ctl());
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(
        host.requests,
        [
            "@s-1 history 8",
            "@s-1 offscreen tail=8000 max=8000 screen=1",
            "@s-1 await frame timeout 61000",
            "@s-1 offscreen tail=8000 max=8000 screen=1"
        ]
    );
    assert_eq!(report.rows, rows(&["❯ second message", "⏺ working on it"]));

    let mut host = Host::default()
        .on("history", no_history())
        .on("offscreen", Read::new(1, &[], whole).ctl());
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(
        host.requests,
        [
            "@s-1 history 8",
            "@s-1 offscreen tail=8000 max=8000 screen=1"
        ]
    );
    assert_eq!(report.rows, rows(&["❯ second message", "⏺ working on it"]));
}

#[test]
fn a_queued_message_under_the_status_row_is_not_a_marker() {
    let screen = claude(
        &[
            "❯ first message",
            "⏺ answer one",
            "❯ second message",
            "⏺ working on it",
            "",
        ],
        &[
            "✶ Deliberating… (4s)",
            "❯ a queued message typed while busy",
        ],
    );
    let read = Read::new(1, &[], screen);
    let mut host = Host::default()
        .on("history", no_history())
        .on("offscreen", read.ctl());
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(
        host.requests,
        [
            "@s-1 history 8",
            "@s-1 offscreen tail=8000 max=8000 screen=1"
        ]
    );
    assert_eq!(
        report.header(),
        "report complete=1 marker=user-row turn=- rows=2 archived=0 screen=2 last=7730:0"
    );
    assert_eq!(report.rows, rows(&["❯ second message", "⏺ working on it"]));

    let mut host = Host::default()
        .on(
            "history",
            history(&[(3, true, "7730:0", "a queued message typed while busy")]),
        )
        .on("offscreen", read.ctl());
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(report.reasons, [Reason::MarkerNotFound]);
    assert_eq!(report.rows.len(), 4, "everything read, from the top");
    let read = Read::new(1, &["", "⏺ a", ""], claude(&["", "⏺ b", ""], &[]));
    let mut host = Host::default()
        .on("history", history(&[(3, true, "7730:0", "nowhere")]))
        .on("offscreen", read.ctl());
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(report.rows, rows(&["⏺ a", "", "", "⏺ b"]));
    assert_eq!((report.archived, report.screen), (2, 2));
}

#[test]
fn a_message_that_starts_with_a_number_is_found() {
    let screen = claude(
        &[
            " Bash command",
            " ❯ 1. Yes",
            "   2. No",
            "❯ 1. Fix the parser",
            "  2. Then rerun the tests",
            "",
            "⏺ Fixing the parser now.",
            "",
        ],
        &[],
    );
    let mut host = Host::default()
        .on(
            "history",
            history(&[
                (
                    5,
                    true,
                    "7730:4",
                    "1. Fix the parser\n2. Then rerun the tests",
                ),
                (6, false, "7730:9", "never landed"),
            ]),
        )
        .on("offscreen", Read::new(5, &[], screen).ctl());
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(
        host.requests[1],
        "@s-1 offscreen since=7730:4 max=8000 screen=1"
    );
    assert_eq!(report.turn, Some(5));
    assert!(report.complete(), "{}", report.header());
    assert_eq!(
        report.rows,
        rows(&[
            "❯ 1. Fix the parser",
            "  2. Then rerun the tests",
            "",
            "⏺ Fixing the parser now.",
        ])
    );
}

#[test]
fn a_long_paste_opens_at_the_last_pasted_row() {
    let text: String = (1..=40)
        .map(|i| format!("step {i} of the plan\n"))
        .collect();
    let screen = claude(
        &[
            "❯ [Pasted text #1 +12 lines]",
            "⏺ Done with the first plan.",
            "❯ [Pasted text #2 +39 lines]",
            "⏺ Starting on step 1.",
        ],
        &[],
    );
    let mut host = Host::default()
        .on("history", history(&[(8, true, "7730:0", text.as_str())]))
        .on("offscreen", Read::new(1, &[], screen).ctl());
    let report = run(&mut host, ReportOpts::default());
    assert!(report.complete(), "{}", report.header());
    assert_eq!(
        report.rows,
        rows(&["❯ [Pasted text #2 +39 lines]", "⏺ Starting on step 1."])
    );
}

#[test]
fn an_old_host_reports_from_the_screen_alone() {
    let screen = claude(&["❯ Keep going", "⏺ Kept going.", ""], &[]);
    let json = {
        let quoted: Vec<String> = screen
            .iter()
            .map(|r| format!("\"{}\"", r.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect();
        format!(
            "{{\"rows\":[{}],\"cursor\":{{\"row\":1,\"col\":2}},\"seq\":5}}\n",
            quoted.join(",")
        )
    };
    let rows_only = CtlReply {
        code: 0,
        stdout: "⏺ some row\n".to_string(),
        stderr: String::new(),
    };
    let status_only = ok(
        "OK 12 first=1 last=5 lost=0 breaks=0 back=0 epoch=1 origin=7730 alt=1 seq=9 \
         screen_rows=7\n",
    );
    for offscreen in [
        err("unknown verb (try: help)"),
        err("denied"),
        rows_only,
        status_only,
    ] {
        let mut host = Host::default()
            .on("history", history(&[(2, true, "7730:0", "Keep going")]))
            .on("offscreen", offscreen)
            .on("text", ok(&json))
            .on("status", status("claude"));
        let report = run(&mut host, ReportOpts::default());
        assert_eq!(host.requests[2], "@s-1 text --json", "the full screen");
        assert_eq!(
            report.header(),
            "report complete=0 reason=no-archive marker=ledger turn=2 rows=2 archived=0 \
             screen=2 last=-"
        );
        assert_eq!(report.rows, rows(&["❯ Keep going", "⏺ Kept going."]));
    }
    let mut host = Host::default()
        .on("history", err("unknown verb (try: help)"))
        .on("offscreen", err("unknown verb (try: help)"))
        .on("text", ok(&json))
        .on("status", err("unknown verb (try: help)"));
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(report.marker, Marker::UserRow);
    assert_eq!(report.reasons, [Reason::NoArchive]);
}

#[test]
fn another_origin_is_an_archive_reset() {
    let mut read = Read::new(
        1,
        &["❯ Keep going", "⏺ Kept going."],
        claude(&["⏺ More."], &[]),
    );
    read.origin = 9999;
    let mut host = Host::default()
        .on("history", history(&[(2, true, "7730:50", "Keep going")]))
        .on("offscreen", read.ctl());
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(report.reasons, [Reason::ArchiveReset]);
    assert_eq!(
        report.last.map(|m| m.to_string()).as_deref(),
        Some("9999:2")
    );
    assert_eq!(
        report.rows,
        rows(&["❯ Keep going", "⏺ Kept going.", "⏺ More."])
    );
}

#[test]
fn a_gap_after_the_mark_counts_only_after_the_start() {
    let archived = ["⏺ before", "❯ Keep going", "⏺ Kept going."];
    let mut read = Read::new(103, &archived, claude(&["⏺ More."], &[]));
    read.breaks = 1;
    for (breaks_after_start, want) in [(0, vec![]), (1, vec![Reason::ArchiveGap])] {
        let mut host = Host::default()
            .on("history", history(&[(2, true, "7730:102", "Keep going")]))
            .on("offscreen", read.ctl())
            .on("offscreen", recheck(breaks_after_start));
        let report = run(&mut host, ReportOpts::default());
        assert_eq!(
            host.requests[2], "@s-1 offscreen since=7730:104 max=1",
            "placed against the user row, archived at 104"
        );
        assert_eq!(report.reasons, want);
        assert_eq!(report.archived, 2);
    }
    let mut read = Read::new(
        103,
        &["⏺ before"],
        claude(&["❯ Keep going", "⏺ Kept."], &[]),
    );
    read.breaks = 2;
    read.lost = 5;
    let mut host = Host::default()
        .on("history", history(&[(2, true, "7730:102", "Keep going")]))
        .on("offscreen", read.ctl());
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(host.requests.len(), 2);
    assert!(report.complete(), "{}", report.header());
    let mut read = Read::new(103, &archived, claude(&["⏺ More."], &[]));
    read.breaks = 1;
    let mut host = Host::default()
        .on("history", history(&[(2, true, "7730:102", "Keep going")]))
        .on("offscreen", read.ctl())
        .on("offscreen", closed());
    let err = Session::new(&mut host, None)
        .report(&ReportOpts::default())
        .expect_err("not served");
    assert!(err.contains("server closed the connection"), "{err}");
}

#[test]
fn since_counts_every_row_after_it() {
    let since = Mark::parse("7730:200");
    let mut read = Read::new(
        203,
        &["⏺ a", "⏺ b"],
        claude(&["⏺ b", "⏺ c", "⏺ d", "⏺ e"], &[]),
    );
    read.lost = 2;
    read.more = true;
    read.back = 3;
    read.back_at = Some(204);
    let mut host = Host::default().on("offscreen", read.ctl());
    let report = run(
        &mut host,
        ReportOpts {
            since,
            max_rows: 2,
            ..ReportOpts::default()
        },
    );
    assert_eq!(
        host.requests,
        ["@s-1 offscreen since=7730:200 max=2 screen=1"]
    );
    assert_eq!(
        report.header(),
        "report complete=0 reason=archive-gap,max-rows marker=since turn=- rows=5 \
         archived=2 screen=3 last=7730:204"
    );
    assert_eq!(report.rows, rows(&["⏺ a", "⏺ b", "⏺ c", "⏺ d", "⏺ e"]));
    let mut read = Read::new(203, &["⏺ a", "⏺ b"], claude(&["⏺ b", "⏺ c"], &[]));
    read.back = 1;
    let mut host = Host::default().on("offscreen", read.ctl());
    let report = run(
        &mut host,
        ReportOpts {
            since,
            ..ReportOpts::default()
        },
    );
    assert!(report.complete(), "{}", report.header());
    assert_eq!(report.rows, rows(&["⏺ a", "⏺ b", "⏺ c"]));
}

#[test]
fn a_cut_page_and_the_main_screen_are_incomplete() {
    let mut read = Read::new(101, &["❯ Keep going", "⏺ one"], claude(&["⏺ ten"], &[]));
    read.more = true;
    let mut host = Host::default()
        .on("history", history(&[(2, true, "7730:100", "Keep going")]))
        .on("offscreen", read.ctl());
    let report = run(
        &mut host,
        ReportOpts {
            since: None,
            max_rows: 2,
            ..ReportOpts::default()
        },
    );
    assert_eq!(report.reasons, [Reason::MaxRows]);
    assert_eq!(report.last.map(|m| m.index), Some(102));

    let mut read = Read::new(1, &[], rows(&["$ claude", "❯ Keep going", "ok", "$"]));
    read.alt = false;
    let mut host = Host::default()
        .on("history", history(&[(2, true, "7730:0", "Keep going")]))
        .on("offscreen", read.ctl())
        .on("status", status("claude"));
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(report.reasons, [Reason::MainScreen]);
    assert_eq!(report.rows, rows(&["❯ Keep going", "ok", "$"]));
}

#[test]
fn a_lost_or_gone_session_is_an_error() {
    let mut host = Host::default().on("history", closed());
    let lost = Session::new(&mut host, None)
        .report(&ReportOpts::default())
        .expect_err("lost");
    assert!(lost.contains("history failed"), "{lost}");
    let mut host = Host::default()
        .on("history", no_history())
        .on("offscreen", err("exited"));
    let e = Session::new(&mut host, None)
        .report(&ReportOpts::default())
        .expect_err("gone");
    assert!(e.contains("offscreen failed: aterm-ctl: ERR exited"), "{e}");
}

#[test]
fn the_readers_match_the_wire() {
    assert_eq!(
        Mark::parse("7730:12"),
        Some(Mark {
            origin: Some(7730),
            index: 12
        })
    );
    assert_eq!(
        Mark::parse("12").map(|m| m.to_string()).as_deref(),
        Some("12")
    );
    for bad in ["", "a", "1:", ":1", "-1", "1:2:3", " 1"] {
        assert_eq!(Mark::parse(bad), None, "{bad:?}");
    }
    let turns = parse_history(
        "OK 3\nturn 4 submitted=1 status=settled started_ms=1 dur_ms=2 seq=3 \
         hash=00000000000000ff by=ctl arch=77:9 text=a%20b%0Ac%25%E2%9D%AF%2\n\
         turn 5 submitted=0 status=timeout text=\n\
         turn 6 submitted=1 status=pressed started_ms=4 dur_ms=5 seq=6 \
         hash=0000000000000001 by=harness arch=77:9 text=keep%20going\n",
    );
    assert_eq!(
        turns,
        [
            LedgerTurn {
                id: 4,
                submitted: true,
                by: "ctl".to_string(),
                arch: Some(Mark {
                    origin: Some(77),
                    index: 9
                }),
                text: "a b\nc%❯%2".to_string(),
            },
            LedgerTurn {
                id: 5,
                submitted: false,
                by: "-".to_string(),
                arch: None,
                text: String::new(),
            },
            LedgerTurn {
                id: 6,
                submitted: true,
                by: "harness".to_string(),
                arch: Some(Mark {
                    origin: Some(77),
                    index: 9
                }),
                text: "keep going".to_string(),
            },
        ]
    );
    assert_eq!(newest_turn(turns.clone()).map(|t| t.id), Some(4));
    assert_eq!(newest_turn(turns[..2].to_vec()).map(|t| t.id), Some(4));
    assert_eq!(newest_turn(turns[1..2].to_vec()).map(|t| t.id), Some(5));
    assert_eq!(
        newest_turn(turns[2..].to_vec()),
        None,
        "the harness's alone"
    );
    assert_eq!(newest_turn(vec![]), None);

    let empty = CtlReply {
        code: 0,
        stdout: String::new(),
        stderr: "aterm-ctl: OK 0 first=1 last=0 lost=0 breaks=0 back=0 epoch=0 origin=5 alt=1 \
                 seq=3 (offscreen: no results)\n"
            .to_string(),
    };
    let off = parse_offscreen(&empty).expect("parses");
    assert_eq!((off.first, off.last, off.origin), (1, 0, 5));
    assert!(off.archived.is_empty() && off.screen.is_empty() && off.alt);
    let read = Read::new(7, &["", "x", ""], rows(&["", ""]));
    let off = parse_offscreen(&read.ctl()).expect("parses");
    assert_eq!(off.archived, rows(&["", "x", ""]));
    assert_eq!(off.screen, rows(&["", ""]));
    assert_eq!(parse_offscreen(&read.raw()), Ok(off));
    let mut short = read.ctl();
    short.stdout.push_str("one too many\n");
    assert!(matches!(
        parse_offscreen(&short),
        Err(OffscreenError::Bad(_))
    ));
    let over = CtlReply {
        stderr: "aterm-ctl: OK 1 first=1 last=1 lost=0 breaks=0 back=0 origin=5 alt=1 \
                 screen_rows=2\n"
            .to_string(),
        ..ok("x\n")
    };
    assert!(matches!(
        parse_offscreen(&over),
        Err(OffscreenError::Bad(_))
    ));
    let no_origin = CtlReply {
        stderr: "aterm-ctl: OK 1 first=1 last=1 lost=0 breaks=0 back=0 alt=1\n".to_string(),
        ..ok("x\n")
    };
    assert!(matches!(
        parse_offscreen(&no_origin),
        Err(OffscreenError::Bad(_))
    ));
    assert_eq!(
        parse_offscreen(&ok("x\ny\n")),
        Err(OffscreenError::NoHeader)
    );
}

#[test]
fn the_transcript_ends_at_the_live_zone() {
    let idle = fixture(super::super::prompt::fixtures::IDLE_AFTER_LIMIT_AND_MODEL_SWITCH);
    let end = transcript_end(&idle);
    assert!(
        idle[end - 1].starts_with("  ⎿  Set model to"),
        "{:?}",
        idle[end - 1]
    );
    let parked = claude(
        &[
            "⏺ Last words.",
            "  ⎿  Tip: a tip inside the transcript stays",
            "⏺ More words.",
            "",
            "  ⎿  Tip: Share Claude Code and earn credits",
            "● How is Claude doing this session? (optional)",
            "  1: Bad    2: Fine   3: Good   0: Dismiss",
            &format!("{:>118}", "✔ Update installed"),
            "",
        ],
        &[],
    );
    assert_eq!(transcript_end(&parked), 3);
    let busy = claude(&["⏺ Working."], &["", "✻ Pushing… (3s)", "  ⎿  Tip: x", ""]);
    assert_eq!(transcript_end(&busy), 2);
    assert_eq!(transcript_end(&rows(&["$ make", "ok", "", ""])), 2);
    assert_eq!(transcript_end(&rows(&["", ""])), 0);
}

#[test]
fn the_paste_fallback_never_claims_an_older_turns_paste() {
    let screen = claude(
        &[
            "❯ [Pasted text #1 +12 lines]",
            "⏺ Starting on step 1 of the plan.",
            "",
        ],
        &["✶ Deliberating… (4s)", "❯ status?"],
    );
    let mut host = Host::default()
        .on(
            "history",
            history(&[
                (7, true, "7730:0", "step 1\nstep 2\nstep 3"),
                (8, true, "7730:0", "status?"),
            ]),
        )
        .on("offscreen", Read::new(1, &[], screen).ctl());
    let report = run(&mut host, ReportOpts::default());
    assert!(
        !report.complete(),
        "turn 8's row is only queued, yet: {}\n{:?}",
        report.header(),
        report.rows
    );
}

#[test]
fn a_numbered_draft_in_the_composer_is_not_the_start() {
    let rule = "─".repeat(120);
    let screen = rows(&[
        "❯ Which approach should we take?",
        "⏺ Two options:",
        "  1. Keep the harness",
        "  2. Rewrite it",
        "",
        rule.as_str(),
        "❯ 1. Keep the harness",
        rule.as_str(),
        "  ⏵⏵ auto mode on (shift+tab to cycle)",
    ]);
    let mut host = Host::default()
        .on("history", no_history())
        .on("offscreen", Read::new(1, &[], screen).ctl());
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(
        report.rows.first().map(String::as_str),
        Some("❯ Which approach should we take?"),
        "{}\n{:?}",
        report.header(),
        report.rows
    );
}

#[test]
fn indented_code_above_an_idle_composer_survives() {
    let screen = claude(
        &[
            "❯ Show me the loop",
            "⏺ The loop now reads:",
            "      for row in rows:",
            "          if row.done:",
            "              for cell in row:",
            "                  if cell.bad:",
            "                      raise ValueError(cell)",
            "",
        ],
        &[],
    );
    assert_eq!(
        transcript_end(&screen),
        7,
        "the last code row {:?} was cut",
        screen[6]
    );
}

#[test]
fn the_done_row_that_ends_a_turn_is_kept() {
    let screen = claude(
        &[
            "❯ Start the load test",
            "⏺ The load test is running detached with the monitor attached.",
            "✻ Worked for 3m 21s · done 8:50 PM · 1 monitor still running",
            "",
        ],
        &[],
    );
    let mut host = Host::default()
        .on(
            "history",
            history(&[(3, true, "7730:0", "Start the load test")]),
        )
        .on("offscreen", Read::new(1, &[], screen).ctl());
    let report = run(&mut host, ReportOpts::default());
    assert!(
        report
            .rows
            .iter()
            .any(|r| r.contains("1 monitor still running")),
        "{:?}",
        report.rows
    );
}

#[test]
fn an_archive_that_is_off_is_no_archive() {
    let screen = claude(&["❯ Keep going", "⏺ Kept going.", ""], &[]);
    let read = Read::new(1, &[], screen);
    let mut ctl = read.ctl();
    ctl.stderr = ctl.stderr.replace(" seq=900", " seq=900 enabled=0");
    let mut host = Host::default()
        .on("history", history(&[(2, true, "7730:0", "Keep going")]))
        .on("offscreen", ctl);
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(report.reasons, [Reason::NoArchive], "{}", report.header());
    assert_eq!(report.rows, rows(&["❯ Keep going", "⏺ Kept going."]));
}

#[test]
fn the_join_skips_the_reshown_rows_under_a_pinned_header() {
    let screen = claude(&["== pinned header ==", "⏺ b", "⏺ c"], &[]);
    let mut read = Read::new(101, &["❯ Go", "⏺ a", "⏺ b"], screen.clone());
    read.back = 1;
    read.pin = 1;
    read.back_at = Some(103);
    let mut host = Host::default()
        .on("history", history(&[(9, true, "7730:100", "Go")]))
        .on("offscreen", read.ctl());
    let report = run(&mut host, ReportOpts::default());
    assert!(report.complete(), "{}", report.header());
    assert_eq!(
        report.rows,
        rows(&["❯ Go", "⏺ a", "⏺ b", "== pinned header ==", "⏺ c"])
    );
    let mut read = Read::new(101, &["❯ Go", "⏺ a"], screen);
    read.back = 1;
    read.pin = 1;
    read.back_at = Some(103);
    read.more = true;
    let mut host = Host::default()
        .on("history", history(&[(9, true, "7730:100", "Go")]))
        .on("offscreen", read.ctl());
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(
        report.rows,
        rows(&["❯ Go", "⏺ a", "== pinned header ==", "⏺ b", "⏺ c"])
    );
}

#[test]
fn a_paste_row_that_does_not_fit_the_turn_is_not_the_start() {
    let text: String = (1..=12).map(|i| format!("step {i}\n")).collect();
    let screen = claude(
        &[
            "❯ [Pasted text #1 +12 lines]",
            "⏺ Done with the first plan.",
            "❯ [Pasted text #2 +39 lines]",
            "⏺ Starting on step 1.",
        ],
        &[],
    );
    let mut host = Host::default()
        .on("history", history(&[(8, true, "7730:0", text.as_str())]))
        .on("offscreen", Read::new(1, &[], screen).ctl());
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(
        report.reasons,
        [Reason::MarkerNotFound],
        "{}",
        report.header()
    );
}

#[test]
fn the_views_keep_the_worker_words_and_drop_the_tool_rows() {
    let screen = fixture(super::super::prompt::fixtures::IDLE_AFTER_LIMIT_AND_MODEL_SWITCH);
    let opts = |view: View| ReportOpts {
        since: Some(Mark {
            origin: Some(7730),
            index: 0,
        }),
        view,
        ..ReportOpts::default()
    };
    let host = || Host::default().on("offscreen", Read::new(1, &[], screen.clone()).ctl());
    let report = run(&mut host(), opts(View::All));
    assert_eq!(report.render_view(View::All), report.render());
    assert!(!report.header().contains("view="), "{}", report.header());

    let final_view = report.render_view(View::Final);
    let (head, body) = final_view.split_once("\n--\n").expect("a header and rows");
    assert_eq!(head, format!("{} view=final kept=3", report.header()));
    assert_eq!(
        body.lines().collect::<Vec<_>>(),
        [
            "⏺ Second-pass reviewers are out and the build is running. Nothing else can start until one of those returns.",
            "",
            "✻ Cooked for 23m 0s · done 11:24 PM",
        ]
    );

    let messages = report.render_view(View::Messages);
    let (head, body) = messages.split_once("\n--\n").expect("a header and rows");
    let kept: Vec<&str> = body.lines().collect();
    assert_eq!(
        head,
        format!("{} view=messages kept={}", report.header(), kept.len())
    );
    for want in [
        "⏺ The reviewers earned their keep: one lens found that the eviction scan honours only the literal --max-entries N spelling, while the",
        "⏺ Second-pass reviewers are out and the build is running. Nothing else can start until one of those returns.",
        "✻ Cooked for 23m 0s · done 11:24 PM",
        "✻ Churned for 0s · done 11:30 PM",
        "❯ /model",
    ] {
        assert!(kept.contains(&want), "the messages view dropped {want:?}");
    }
    for gone in [
        "  Ran 1 shell command",
        "  Ran 3 shell commands",
        "⏺ Stop Task",
        "✻ Waiting for 1 dynamic workflow to finish",
    ] {
        assert!(!kept.contains(&gone), "the messages view kept {gone:?}");
    }
    for row in &kept {
        assert!(
            !row.trim_start().starts_with('⎿')
                && !row.starts_with("⏺ Workflow(")
                && !row.starts_with("⏺ Dynamic workflow")
                && !row.starts_with("⏺ Background command"),
            "the messages view kept a tool row: {row:?}"
        );
    }
    let mut at = 0;
    for row in &kept {
        if row.is_empty() {
            continue;
        }
        at = report.rows[at..]
            .iter()
            .position(|r| r == row)
            .map(|i| at + i + 1)
            .unwrap_or_else(|| panic!("{row:?} is not a report row, or is out of order"));
    }
}

#[test]
fn a_final_view_of_a_headless_message_keeps_its_words_and_its_done_row() {
    let screen = fixture(super::super::prompt::fixtures::WAIT_BG3);
    let mut host = Host::default().on("offscreen", Read::new(1, &[], screen).ctl());
    let report = run(
        &mut host,
        ReportOpts {
            since: Some(Mark {
                origin: Some(7730),
                index: 0,
            }),
            view: View::Final,
            ..ReportOpts::default()
        },
    );
    let text = report.render_view(View::Final);
    let (head, body) = text.split_once("\n--\n").expect("a header and rows");
    assert!(head.contains(" view=final kept="), "{head}");
    let kept: Vec<&str> = body.lines().collect();
    assert!(
        kept[0].starts_with("  └──────────"),
        "the table border: {:?}",
        kept[0]
    );
    assert!(
        kept.iter()
            .any(|r| r.starts_with("  - Sets B and C: key interning")),
        "the bullets are kept"
    );
    assert!(
        kept.iter().any(|r| r.trim() == "Waiting for your call."),
        "the last words are kept"
    );
    assert_eq!(
        kept.last().copied(),
        Some("✻ Sautéed for 50m 35s · done 10:57 AM")
    );
    assert!(
        !kept.iter().any(|r| r.contains("⏵⏵ auto mode on")),
        "the footer is not a report row"
    );
}
