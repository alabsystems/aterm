// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `report`, over a scripted host: the joins, the start, every reason, and the
//! requests it makes. Screens are synthetic, shaped like Claude Code's, or
//! Codex's measured ones.

use std::collections::{BTreeMap, VecDeque};

use super::super::blocks::View;
use super::super::phase::transcript_end;
use super::super::prompt::fixtures::{composer, rows};
use super::*;

/// A scripted host: replies per request or verb, in order; every request
/// recorded. A host given a MAIN screen ([`Self::main`]) answers `line <row>`
/// itself, by the row asked, once its scripted `line` replies are spent.
#[derive(Default)]
struct Host {
    replies: BTreeMap<String, VecDeque<CtlReply>>,
    requests: Vec<String>,
    /// The main screen and its scrollback by absolute row, and the oldest row
    /// kept (`ERR evicted` below it; `ERR out of range` past the last).
    main: Option<(BTreeMap<u64, String>, u64)>,
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
        if verb == "line"
            && let Some((main, oldest)) = &self.main
            && let Some(at) = args.last().and_then(|a| a.parse::<u64>().ok())
        {
            return Ok(match main.get(&at) {
                _ if at < *oldest => err("evicted"),
                Some(row) => ok(&format!("OK {row}\n")),
                None => err("out of range"),
            });
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

/// A Claude Code screen: the transcript, the live zone under it (a status
/// row and what hangs under it), the composer frame and a footer.
fn claude(transcript: &[&str], live: &[&str]) -> Vec<String> {
    let mut r = rows(transcript);
    r.extend(rows(live));
    r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
    r
}

fn fixture(text: &str) -> Vec<String> {
    let mut r: Vec<String> = text.lines().map(str::to_string).collect();
    // The capture's own first and last lines are not the screen.
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

/// The whole path on a worker whose turn scrolled off the top: the ledger's
/// newest submitted turn gives the mark and the text, one `offscreen` read
/// gives the archive after the mark and the screen, the screen's first
/// `back` rows (already archived) are skipped, the live zone is cut by
/// position, and the report opens at the user row — wrapped, so its first 60
/// characters span two rows. Transcript rows `is_said` would drop — a done
/// row, a table, a todo item, indented code — are all kept.
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
            // The done row that ends the turn is the transcript's, not the
            // live zone's: kept.
            "✻ Cooked for 4s · done 2:41 PM",
        ])
    );
    assert_eq!(
        report.render(),
        format!("{}\n--\n{}\n", report.header(), report.rows.join("\n"))
    );
    // The same read through a raw client (the header as stdout's first line)
    // is the same report.
    let mut host = Host::default()
        .on("history", history(&[(42, true, "7730:100", LEVER)]))
        .on("offscreen", read.raw());
    assert_eq!(run(&mut host, ReportOpts::default()), report);
}

/// The saved screen of a real worker whose user block wrapped over seven rows
/// (wait_bg2): nothing was archived yet, the status row `✶ Deliberating…` and
/// the tip under it are the live zone, and the report is the user block.
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

/// Without a ledger turn (`drive prompt` keeps none) the report opens at the
/// last `❯` row of the transcript — never at a message queued under the
/// status row, which is the live zone. And a ledger turn whose text is only
/// in that queue has no row yet: marker-not-found.
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
    // Blank rows at either end are not rows the report holds.
    let read = Read::new(1, &["", "⏺ a", ""], claude(&["", "⏺ b", ""], &[]));
    let mut host = Host::default()
        .on("history", history(&[(3, true, "7730:0", "nowhere")]))
        .on("offscreen", read.ctl());
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(report.rows, rows(&["⏺ a", "", "", "⏺ b"]));
    assert_eq!((report.archived, report.screen), (2, 2));
}

/// A message that starts with `1.` is a user row like any other (a prompt's
/// options are indented, `❯ 1. Yes`, and never one), and the newest turn
/// whose submit did not land is passed over for the one before it.
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

/// A long paste shows as `[Pasted text #N …]`, which the ledger's text never
/// matches: the report opens at the last such row (the one above it on the
/// screen is the turn before).
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

/// A host that predates `offscreen` answers `ERR unknown verb` (`ERR denied`
/// when the verb is aimed at another session: an unknown verb has no op to
/// authorize), and an `aterm ctl` that relays no header gives rows that
/// cannot be placed: the report falls back to the whole screen, no-archive.
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
    for offscreen in [err("unknown verb (try: help)"), err("denied"), rows_only] {
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
    // A host that has neither `history` nor `offscreen` (nor `status`).
    let mut host = Host::default()
        .on("history", err("unknown verb (try: help)"))
        .on("offscreen", err("unknown verb (try: help)"))
        .on("text", ok(&json))
        .on("status", err("unknown verb (try: help)"));
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(report.marker, Marker::UserRow);
    assert_eq!(report.reasons, [Reason::NoArchive]);
}

/// The archive is not the one the mark named (a restart, a handoff): the host
/// reads from its start, and the report says archive-reset.
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

/// A gap counted after the mark is placed against the start with one more
/// read: one at the mark itself (a resize just before the turn) leaves the
/// report complete; one after the start makes it archive-gap. A start on the
/// screen needs no second read — every row came from one snapshot.
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
    // The start on the screen: no second read.
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
    // A lost connection on the second read is not a gap: it is an error.
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

/// `--since <origin:i>` opens right after that row: every row counts, so a
/// row evicted since, a gap anywhere after it and a page cut short by
/// `--max-rows` each make it incomplete. A page cut short keeps the rows at
/// the top of the screen it did not read.
#[test]
fn since_counts_every_row_after_it() {
    let since = Mark::parse("7730:200");
    // Rows 203..=206 were archived; the page held 203 and 204 (`last=` is the
    // page's own last row). The screen's top three rows are 204..=206: 204 was
    // read, the other two were not.
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
    // Read to the end, the `back` rows at the top of the screen are skipped.
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

/// The ledger's page cut short by `--max-rows` misses the middle (max-rows);
/// a worker off the alternate screen that draws no main-screen transcript
/// this report reads (Claude Code's inline renderer) is main-screen.
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

/// A request the host did not serve is an error, never an empty report; so is
/// a session that is gone.
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

/// The readers, piece by piece: a mark, a `history` line (pct-decoded, cut
/// at ` text=`), an `offscreen` header in each place a client leaves it and
/// every way one can be wrong.
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
        "OK 2\nturn 4 submitted=1 status=settled started_ms=1 dur_ms=2 seq=3 \
         hash=00000000000000ff arch=77:9 text=a%20b%0Ac%25%E2%9D%AF%2\n\
         turn 5 submitted=0 status=timeout text=\n",
    );
    assert_eq!(
        turns,
        [
            LedgerTurn {
                id: 4,
                submitted: true,
                arch: Some(Mark {
                    origin: Some(77),
                    index: 9
                }),
                text: "a b\nc%❯%2".to_string(),
            },
            LedgerTurn {
                id: 5,
                submitted: false,
                arch: None,
                text: String::new(),
            },
        ]
    );
    assert_eq!(newest_turn(turns.clone()).map(|t| t.id), Some(4));
    assert_eq!(newest_turn(turns[1..].to_vec()).map(|t| t.id), Some(5));
    assert_eq!(newest_turn(vec![]), None);

    // An empty archive and screen: `aterm ctl` prints the header with its
    // no-results note.
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
    // Blank rows are rows.
    let read = Read::new(7, &["", "x", ""], rows(&["", ""]));
    let off = parse_offscreen(&read.ctl()).expect("parses");
    assert_eq!(off.archived, rows(&["", "x", ""]));
    assert_eq!(off.screen, rows(&["", ""]));
    assert_eq!(parse_offscreen(&read.raw()), Ok(off));
    // What does not add up.
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

/// Where the transcript ends, by position only: at the status row; above an
/// idle composer, above the hints, tips, survey and blanks parked there (a
/// transcript row that looks like them further up stays); without a frame,
/// after the last non-blank row.
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
            // Parked against the right edge: two columns short of the rule.
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

// -------------------------------------------- report opening and row edge cases

/// A short turn is never a paste: with an earlier turn's paste on the screen
/// and a short turn queued under the status row, the report does not open at
/// the earlier paste.
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

/// A composer holding a draft that starts with `1.` (an answer to the
/// worker's numbered question, typed but not sent) is the composer, not an
/// option row: the caret in column 0 under a composer rule is the composer,
/// whatever it holds, so the draft, the rule and the footer are not report.
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

/// Above an idle composer (no done row), the worker's last transcript row is
/// kept when indented 20+ columns (indented code): a hint is parked against
/// the right edge; code is not.
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

/// The done row that ends an idle turn (here one saying a monitor is still
/// running) is kept verbatim, not taken for the status row and cut.
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

/// An `aterm ctl` that predates `offscreen` frames an unknown verb as a
/// STATUS line: it prints `OK <n> first=…` on stdout and drops the rows. The
/// report is no-archive and the screen alone, as DRIVE_HELP promises, never
/// a header-count failure.
#[test]
fn an_aterm_ctl_that_frames_offscreen_as_status_falls_back() {
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
    let status_only = ok(
        "OK 12 first=1 last=5 lost=0 breaks=0 back=0 epoch=1 origin=7730 alt=1 seq=9 \
         screen_rows=7\n",
    );
    let mut host = Host::default()
        .on("history", history(&[(2, true, "7730:0", "Keep going")]))
        .on("offscreen", status_only)
        .on("text", ok(&json))
        .on("status", status("claude"));
    let report = Session::new(&mut host, Some("@s-1".to_string())).report(&ReportOpts::default());
    assert!(
        report
            .as_ref()
            .is_ok_and(|r| r.reasons.contains(&Reason::NoArchive)),
        "{report:?}"
    );
}

/// A host whose archive is off (`enabled=0`) kept nothing that scrolled away:
/// the report is the screen's, and says no-archive even with the start found.
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

/// Under a pinned header the rows the screen shows again from the archive
/// sit at `pin..pin+back` (`back_at=` says which): those are skipped, the
/// header above them is not — and a re-shown row this read did not get is
/// kept from the screen.
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
    // `back_at=` past what the page got: nothing is skipped.
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

/// The last paste row is the newest paste turn's only when its line count
/// fits the text; one that does not is another turn's, and an older paste
/// row further up is never reached for.
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

// ------------------------------------------- the report's two views

/// The `--final` and `--messages` views on the saved screen of a real worker
/// (`idle-after-limit-and-model-switch`, the fixture
/// [`the_transcript_ends_at_the_live_zone`] reads): the default report is
/// byte-identical to the view-less report, `--final` is the worker's last
/// message and the done row that ended the turn, and `--messages` keeps every
/// message block, the user's `❯` row and the done rows while dropping every
/// tool row — a collapsed `Ran N shell commands` group, a `⏺ Stop Task`, a
/// `⏺ Workflow(…)` call, Claude Code's `Dynamic workflow`/`Background
/// command` notices, the `✻ Waiting for …` status row — and all their `⎿`
/// output.
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
    // The default is unchanged: the same bytes, no `view=` in the header.
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
    // Every kept row is a row the report holds, in its order.
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

/// `--final` on the saved screen of a worker whose message began before the
/// screen did (`wait_bg3`: the head scrolled off, so the rows open with the
/// message's own wrapped rows): the words are kept — the table border, the
/// bullets, the recommendation — and the done row that ended the turn with
/// them, while the composer, its footer and the live zone are not report rows
/// at all.
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

// ---- A worker on the MAIN screen: Codex ----------------------------------

/// The Codex turn of the main-screen tests: the ledger's text.
const CODEX_TURN: &str = "Fix the bug in calc.py, then say what it was.";

/// The main screen and its scrollback of a Codex worker whose turn
/// [`CODEX_TURN`] scrolled off, by absolute row (rows 10 to 20 the
/// scrollback's, 21 to 29 the screen's): an earlier turn, the user row at 16,
/// the answer, its end row, the composer and its footer. Codex's layout
/// ([`aterm_phase::codex`]): `›` user rows, `•` blocks, a bare clock ending
/// the turn.
fn codex_main() -> Vec<(u64, String)> {
    let rows = [
        "› Tell me about tests.",
        "",
        "• Tests pin behaviour.",
        "",
        "  1:20 PM",
        "",
        "› Fix the bug in calc.py, then say what it was.",
        "",
        "• I'll inspect calc.py first.",
        "  └ Read calc.py",
        "",
        "• The bug was an off-by-one in subtract.",
        "  Fixed it and ran the tests.",
        "",
        "  1:39 PM",
        "",
        "",
        "› Ask Codex to do anything",
        "",
        "  GPT-5.6-Luna low · /tmp/work",
    ];
    (10..).zip(rows.iter().map(|r| r.to_string())).collect()
}

/// The screen of [`codex_main`]: its rows 21 to 29.
fn codex_screen() -> Vec<String> {
    codex_main()
        .into_iter()
        .filter(|(at, _)| *at >= 21)
        .map(|(_, r)| r)
        .collect()
}

/// A host serving [`codex_main`] off the alternate screen: the ledger's turn
/// [`CODEX_TURN`], `status` naming `program`, `search ^› regex` naming its
/// `›` rows, and `line <row>` answering each row of [`codex_main`] by its
/// number (`ERR out of range` past the last).
fn codex_host(program: &str, screen: Vec<String>) -> Host {
    let mut read = Read::new(1, &[], screen);
    read.alt = false;
    let mut host = Host::default()
        .on("history", history(&[(42, true, "7730:100", CODEX_TURN)]))
        .on("offscreen", read.ctl())
        .on("status", status(program))
        .on(
            "search",
            crate::supervise::transport::render(
                "search",
                aterm_types::control_verbs::Framing::Lines,
                "OK 3 screen_first=21",
                b"10 0 1\n16 0 1\n27 0 1\n".as_slice(),
            ),
        );
    host.main = Some((codex_main().into_iter().collect(), 0));
    host
}

/// A CODEX worker's turn still on its screen: the report is the screen's,
/// from its `›` user row to the composer, and complete — nothing scrolled
/// off. Negative control: read as the session's program names it otherwise
/// (`claude`, as every main-screen worker was read before), the same report
/// is incomplete: main-screen, and no `❯` row to start at.
#[test]
fn a_codex_turn_on_its_screen_is_the_screens_report() {
    let screen: Vec<String> = codex_main()
        .into_iter()
        .filter(|(at, _)| *at >= 16 && *at != 18 && *at != 19)
        .map(|(_, r)| r)
        .collect();
    let mut host = codex_host("codex", screen.clone());
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(
        report.header(),
        "report complete=1 marker=ledger turn=42 rows=7 archived=0 screen=7 last=-"
    );
    assert_eq!(
        report.rows,
        rows(&[
            "› Fix the bug in calc.py, then say what it was.",
            "",
            "",
            "• The bug was an off-by-one in subtract.",
            "  Fixed it and ran the tests.",
            "",
            "  1:39 PM",
        ])
    );
    assert_eq!(
        host.requests,
        [
            "@s-1 history 8",
            "@s-1 offscreen since=7730:100 max=8000 screen=1",
            "@s-1 status",
        ]
    );
    let mut host = codex_host("claude", screen);
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(
        report.reasons,
        [Reason::MainScreen, Reason::MarkerNotFound],
        "{}",
        report.header()
    );
}

/// A CODEX worker's turn scrolled off its screen into the main screen's
/// scrollback: `search ^› regex` names its user rows, the newest above the
/// screen whose text begins with the turn's is the start (row 16; the
/// screen's own `›` row, the composer, is skipped), and every row from it is
/// read by `line` down past the screen's last, the composer and its footer
/// cut. Complete: nothing between the start and the end was lost. Without a
/// ledger turn, the newest user row above the screen is the start.
#[test]
fn a_codex_turn_that_scrolled_off_is_read_back_from_the_scrollback() {
    let want = rows(&[
        "› Fix the bug in calc.py, then say what it was.",
        "",
        "• I'll inspect calc.py first.",
        "  └ Read calc.py",
        "",
        "• The bug was an off-by-one in subtract.",
        "  Fixed it and ran the tests.",
        "",
        "  1:39 PM",
    ]);
    let lines: Vec<u64> = (16..=30).collect();
    let mut host = codex_host("codex", codex_screen());
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(
        report.header(),
        "report complete=1 marker=ledger turn=42 rows=9 archived=5 screen=4 last=-"
    );
    assert_eq!(report.rows, want);
    let mut asked = vec![
        "@s-1 history 8".to_string(),
        "@s-1 offscreen since=7730:100 max=8000 screen=1".to_string(),
        "@s-1 status".to_string(),
        "@s-1 search ^› regex".to_string(),
    ];
    asked.extend(lines.iter().map(|at| format!("@s-1 line {at}")));
    assert_eq!(host.requests, asked);

    let mut host = codex_host("codex", codex_screen());
    host.replies
        .insert("history".to_string(), VecDeque::from([no_history()]));
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(
        report.header(),
        "report complete=1 marker=user-row turn=- rows=9 archived=5 screen=4 last=-"
    );
    assert_eq!(report.rows, want);
}

/// An oldest-first search cut short can omit newer user rows and the
/// composer. Subtracting the composer's count from that list would drop a
/// real user hit and report an older turn as complete. Other partial or
/// unknown search causes are also refused; depth-only truncation still
/// includes the newest range and can supply the report's start.
#[test]
fn a_codex_report_rejects_searches_that_may_hide_newer_user_rows() {
    use aterm_types::control_verbs::Framing;

    for qualifier in [
        "incomplete capped",
        "incomplete changed",
        "incomplete rewrapping",
        "incomplete long-line",
        "incomplete future-cause",
        "future-marker",
    ] {
        let header = format!("OK 2 {qualifier} screen_first=21");
        for search in [
            crate::supervise::transport::render(
                "search",
                Framing::Lines,
                &header,
                b"10 0 1\n16 0 1\n".as_slice(),
            ),
            ok(&format!("{header}\n10 0 1\n16 0 1\n")),
        ] {
            let mut host = codex_host("codex", codex_screen());
            host.replies
                .insert("history".into(), VecDeque::from([no_history()]));
            host.replies
                .insert("search".into(), VecDeque::from([search]));
            let report = run(&mut host, ReportOpts::default());
            assert_eq!(
                report.reasons,
                [Reason::SearchIncomplete, Reason::MarkerNotFound],
                "{qualifier}: {}",
                report.header()
            );
            assert_eq!(
                report.rows.first().map(String::as_str),
                Some("• The bug was an off-by-one in subtract.")
            );
            assert!(
                !host.requests.iter().any(|r| r.contains(" line ")),
                "{:#?}",
                host.requests
            );
        }
    }

    for qualifier in ["", " incomplete", " incomplete older"] {
        let mut host = codex_host("codex", codex_screen());
        host.replies.insert(
            "search".into(),
            VecDeque::from([crate::supervise::transport::render(
                "search",
                Framing::Lines,
                &format!("OK 3{qualifier} screen_first=21"),
                b"10 0 1\n16 0 1\n27 0 1\n".as_slice(),
            )]),
        );
        let report = run(&mut host, ReportOpts::default());
        assert!(report.complete(), "{qualifier}: {}", report.header());
        assert_eq!(
            report.rows.first().map(String::as_str),
            Some("› Fix the bug in calc.py, then say what it was.")
        );
    }

    // Missing or malformed position metadata cannot place the live boundary.
    for header in [
        "OK 2",
        "OK 2 screen_first=bad",
        "OK 2 screen_first=21 screen_first=22",
    ] {
        let mut host = codex_host("codex", codex_screen());
        host.replies.insert(
            "search".into(),
            VecDeque::from([crate::supervise::transport::render(
                "search",
                Framing::Lines,
                header,
                b"10 0 1\n16 0 1\n".as_slice(),
            )]),
        );
        let report = run(&mut host, ReportOpts::default());
        assert!(
            report.reasons.contains(&Reason::SearchIncomplete),
            "{header}"
        );
    }

    // A legacy client's human-readable incomplete note is no machine
    // contract, even when its words happen to describe a depth limit.
    for note in [
        "results stop at 100000 matches",
        "screen only: the history is rewrapping",
        "a long line was not searched to its end: the regex ran out of scan budget there",
        "older history not searched (search_history_lines)",
    ] {
        let mut host = codex_host("codex", codex_screen());
        let mut search = ok("10 0 1\n16 0 1\n");
        search.stderr = format!("aterm-ctl: {note}\n");
        host.replies
            .insert("search".into(), VecDeque::from([search]));
        let report = run(&mut host, ReportOpts::default());
        assert!(report.reasons.contains(&Reason::SearchIncomplete));
    }
}

/// A continuation row beginning with the caret is no logical-line `^`
/// match. It must not cause the report to discard a genuine historical hit.
#[test]
fn a_codex_report_places_live_hits_by_coordinate_instead_of_physical_caret_count() {
    let mut main = codex_main();
    main.iter_mut().find(|(at, _)| *at == 20).unwrap().1 = "• a wrapped output row".into();
    main.iter_mut().find(|(at, _)| *at == 21).unwrap().1 = "›its continuation".into();
    // The older turn has the same requested prefix: choosing it would
    // report an extra turn as complete instead of just the requested one.
    main.iter_mut().find(|(at, _)| *at == 10).unwrap().1 = format!("› {CODEX_TURN}");
    let screen = main
        .iter()
        .filter(|(at, _)| *at >= 21)
        .map(|(_, row)| row.clone())
        .collect();
    let mut host = codex_host("codex", screen);
    for (at, row) in main {
        host = host.on(&format!("line {at}"), ok(&format!("OK {row}\n")));
    }
    host = host.on("line", err("out of range"));
    let report = run(&mut host, ReportOpts::default());
    assert!(report.complete(), "{}", report.header());
    assert_eq!(
        report.rows.first().map(String::as_str),
        Some("› Fix the bug in calc.py, then say what it was.")
    );
    assert!(
        !report
            .rows
            .iter()
            .any(|row| row == "• Tests pin behaviour."),
        "{:?}",
        report.rows
    );
    assert!(report.rows.iter().any(|row| row == "›its continuation"));
}

/// What keeps a CODEX worker's report from being whole is said: a host
/// without `search` gives the screen alone (main-screen, and no start on
/// it); a start evicted from the scrollback is a gap and not found; a turn no
/// user row begins with is not found, each user row above the screen read as
/// far as its block could still begin with the turn (row 10's ends at row
/// 12, short of it). (`--max-rows`:
/// [`a_codex_report_cut_by_max_rows_keeps_the_turns_end`]; an incomplete
/// `search`: [`a_codex_report_says_where_the_search_was_incomplete`].)
#[test]
fn a_codex_report_names_what_it_could_not_read() {
    let screen_rows = rows(&[
        "• The bug was an off-by-one in subtract.",
        "  Fixed it and ran the tests.",
        "",
        "  1:39 PM",
    ]);
    let mut host = codex_host("codex", codex_screen());
    host.replies.insert(
        "search".to_string(),
        VecDeque::from([err("unknown verb (try: help)")]),
    );
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(
        report.header(),
        "report complete=0 reason=main-screen,marker-not-found marker=ledger turn=42 rows=4 \
         archived=0 screen=4 last=-"
    );
    assert_eq!(report.rows, screen_rows);

    let mut host = codex_host("codex", codex_screen());
    if let Some((_, oldest)) = host.main.as_mut() {
        *oldest = 17;
    }
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(
        report.reasons,
        [Reason::ArchiveGap, Reason::MarkerNotFound],
        "{}",
        report.header()
    );
    assert_eq!(report.rows, screen_rows);

    let mut host = codex_host("codex", codex_screen());
    host.replies.insert(
        "history".to_string(),
        VecDeque::from([history(&[(
            42,
            true,
            "7730:100",
            "Something else entirely",
        )])]),
    );
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(
        report.reasons,
        [Reason::MarkerNotFound],
        "{}",
        report.header()
    );
    assert_eq!(
        host.requests[4..],
        [
            "@s-1 line 16",
            "@s-1 line 10",
            "@s-1 line 11",
            "@s-1 line 12"
        ],
        "{:#?}",
        host.requests
    );

    // An unreadable retained row is a request failure, never the live
    // bottom: treating it as out-of-range would declare this prefix complete.
    let mut host = codex_host("codex", codex_screen());
    host = host.on("line 17", err("unreadable"));
    let error = Session::new(&mut host, Some("@s-1".into()))
        .report(&ReportOpts::default())
        .expect_err("the unreadable middle cannot end a complete report");
    assert!(error.contains("unreadable"), "{error}");
}

/// A CODEX worker's report whose `--max-rows` runs out before the screen
/// KEEPS THE TURN'S END, as the archive's page cut short keeps the screen:
/// the rows read from the start (16 to 18), then the screen's (21 on, its
/// last row found by `line` probes at twice the step, then halving: 19, 21,
/// 25, 33 past it, 29, 31, 30), the middle missing (max-rows). Spent on the
/// screen (at row 24), the screen's rows from there close the report and
/// nothing is missing: it is the unbounded report. Without the end kept, the
/// report stopped at the rows read and its end row (`1:39 PM`) was gone.
#[test]
fn a_codex_report_cut_by_max_rows_keeps_the_turns_end() {
    let mut host = codex_host("codex", codex_screen());
    let report = run(
        &mut host,
        ReportOpts {
            max_rows: 3,
            ..ReportOpts::default()
        },
    );
    assert_eq!(
        report.header(),
        "report complete=0 reason=max-rows marker=ledger turn=42 rows=7 archived=3 screen=4 \
         last=-"
    );
    assert_eq!(
        report.rows,
        rows(&[
            "› Fix the bug in calc.py, then say what it was.",
            "",
            "• I'll inspect calc.py first.",
            "• The bug was an off-by-one in subtract.",
            "  Fixed it and ran the tests.",
            "",
            "  1:39 PM",
        ])
    );
    assert_eq!(
        host.requests[4..],
        [16, 17, 18, 19, 21, 25, 33, 29, 31, 30].map(|at| format!("@s-1 line {at}")),
        "{:#?}",
        host.requests
    );

    let mut host = codex_host("codex", codex_screen());
    let report = run(
        &mut host,
        ReportOpts {
            max_rows: 8,
            ..ReportOpts::default()
        },
    );
    assert_eq!(
        report.header(),
        "report complete=1 marker=ledger turn=42 rows=9 archived=5 screen=4 last=-"
    );
    assert_eq!(report.rows.last().map(String::as_str), Some("  1:39 PM"));
}

/// A `search` that says it is ` incomplete` is said where the start may lie
/// among the rows it left out (search-incomplete): the depth cap alone
/// (`aterm ctl`'s note, a raw client's bare ` incomplete` or ` older`)
/// leaves out only rows OLDER than the ones searched, so a start found is the
/// newest match and the report is whole, and a start not found may be among
/// them; the match cap, a rewrap or a long line leaves out rows anywhere, so
/// even a start found may not be the newest. Every reply carries the header
/// `screen_first=` placing the live screen (one without it is refused:
/// [`a_codex_report_rejects_searches_that_may_hide_newer_user_rows`]), as
/// `aterm ctl` prints it (its note on stderr, then the header) or as a raw
/// client reads it (the header on stdout's first line). Negative control: a
/// complete search that does not find the start is marker-not-found alone.
#[test]
fn a_codex_report_says_where_the_search_was_incomplete() {
    let hits = "10 0 1\n16 0 1\n27 0 1\n";
    let note = |text: &str, header: &str| CtlReply {
        code: 0,
        stdout: hits.to_string(),
        stderr: format!("aterm-ctl: {text}\naterm-ctl: {header} screen_first=21\n"),
    };
    let raw = |header: &str| ok(&format!("{header} screen_first=21\n{hits}"));
    let both = format!("results stop at 100000 matches; {SEARCH_DEPTH_NOTE}");
    for (search, found, want) in [
        (note(SEARCH_DEPTH_NOTE, "OK 3 incomplete"), true, vec![]),
        (raw("OK 3 incomplete"), true, vec![]),
        (raw("OK 3 incomplete older"), true, vec![]),
        (
            note(&both, "OK 3 incomplete capped older"),
            true,
            vec![Reason::SearchIncomplete],
        ),
        (
            note(
                "screen only: the history is rewrapping",
                "OK 3 incomplete rewrapping",
            ),
            true,
            vec![Reason::SearchIncomplete],
        ),
        (
            raw("OK 3 incomplete long-line older"),
            true,
            vec![Reason::SearchIncomplete],
        ),
        (
            raw("OK 100000 incomplete"),
            true,
            vec![Reason::SearchIncomplete],
        ),
        (
            note(SEARCH_DEPTH_NOTE, "OK 3 incomplete"),
            false,
            vec![Reason::SearchIncomplete, Reason::MarkerNotFound],
        ),
        (raw("OK 3"), false, vec![Reason::MarkerNotFound]),
    ] {
        let said = format!("{search:?}");
        let mut host = codex_host("codex", codex_screen());
        host.replies
            .insert("search".to_string(), VecDeque::from([search]));
        if !found {
            host.replies.insert(
                "history".to_string(),
                VecDeque::from([history(&[(42, true, "7730:100", "Something else")])]),
            );
        }
        let report = run(&mut host, ReportOpts::default());
        assert_eq!(report.reasons, want, "{said}: {}", report.header());
    }
}

/// What a `search` reply says it left out, read as `aterm ctl` prints it: a
/// reply with no hits carries its header on stderr with the client's own
/// note after it (`OK 0 incomplete older (search: no results)`), and that
/// note is no cause — the depth cap alone is still OLDER history only. Read
/// as causes, the note's words made it rows left out anywhere. NEGATIVE
/// CONTROLS: a cause beside the depth cap is anywhere, and a complete reply
/// with no hits left nothing out.
#[test]
fn a_search_with_no_hits_says_what_it_left_out_by_its_causes_alone() {
    let said = |stderr: &str| {
        unsearched(&CtlReply {
            code: 0,
            stdout: String::new(),
            stderr: stderr.to_string(),
        })
    };
    assert_eq!(
        said(&format!(
            "aterm-ctl: {SEARCH_DEPTH_NOTE}\naterm-ctl: OK 0 incomplete older (search: no results)\n"
        )),
        Unsearched::Older
    );
    assert_eq!(
        said("aterm-ctl: OK 0 incomplete (search: no results)\n"),
        Unsearched::Older
    );
    assert_eq!(
        said("aterm-ctl: OK 0 incomplete rewrapping older (search: no results)\n"),
        Unsearched::Anywhere
    );
    assert_eq!(
        said("aterm-ctl: OK 0 (search: no results)\n"),
        Unsearched::Nothing
    );
}

/// A CODEX worker still at work: its report ends where its LIVE ZONE begins —
/// the status row (`• Working (6s • esc to interrupt)`, measured on 0.156.1
/// and 0.163.0), the composer and the footer are not the worker's words, as
/// Claude Code's spinner row and composer are cut from its report. Without
/// the reader's cut, the status row stood in the report as its last row.
/// Negative control: the same turn ended (its status row an end row) keeps
/// that end row.
#[test]
fn a_busy_codex_workers_report_ends_above_its_status_row() {
    use aterm_phase::codex::fixtures as cx;
    let screen = aterm_phase::prompt::fixtures::screen;
    let turn = "Fix the bug in calc.py so add returns a + b. Edit the file with a patch; do not \
                run anything.";
    let busy = screen(cx::BUSY);
    let mut host = codex_host("codex", busy.clone());
    host.replies.insert(
        "history".to_string(),
        VecDeque::from([history(&[(42, true, "7730:100", turn)])]),
    );
    let report = run(&mut host, ReportOpts::default());
    assert!(report.complete(), "{}", report.header());
    assert_eq!(
        report.rows.last().map(String::as_str),
        Some("  └ Read calc.py"),
        "{:#?}",
        report.rows
    );
    assert!(
        !report.rows.iter().any(|r| r.contains("esc to interrupt")),
        "{:#?}",
        report.rows
    );

    let mut ended = busy;
    let at = ended
        .iter()
        .position(|r| r.starts_with("• Working ("))
        .expect("the status row");
    ended[at] = "  1:40 PM".to_string();
    let mut host = codex_host("codex", ended);
    host.replies.insert(
        "history".to_string(),
        VecDeque::from([history(&[(42, true, "7730:100", turn)])]),
    );
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(report.rows.last().map(String::as_str), Some("  1:40 PM"));

    let busy = screen(cx::BUSY_0_163);
    let mut host = codex_host("codex", busy);
    host.replies.insert(
        "history".to_string(),
        VecDeque::from([history(&[(
            42,
            true,
            "7730:100",
            "Reply with exactly the word hi and nothing else.",
        )])]),
    );
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(
        report.rows,
        rows(&["› Reply with exactly the word hi and nothing else."]),
        "{}",
        report.header()
    );
}

/// `--final` and `--messages` on a CODEX worker's report read it in Codex's
/// grammar (measured screens, 0.156.1 and 0.163.0): its `•` messages and `›`
/// rows and the end rows that ended its turns, never a tool block (`•
/// Explored` and its `└` output, `• Edited` and its diff rows, `• Failed
/// (exit 1) …`), an approval's echo (`✗ You canceled …`) or a status row; a
/// turn ended on a `■` block keeps that block. Negative control: the same
/// reports read in Claude Code's grammar — what `report` did — keep nothing.
#[test]
fn a_codex_reports_views_keep_what_it_said() {
    use aterm_phase::codex::fixtures as cx;
    let screen = aterm_phase::prompt::fixtures::screen;
    let report_of = |text: &str, turn: &str| {
        let mut host = codex_host("codex", screen(text));
        host.replies.insert(
            "history".to_string(),
            VecDeque::from([history(&[(42, true, "7730:100", turn)])]),
        );
        run(&mut host, ReportOpts::default())
    };
    let kept = |report: &Report, view: View| -> Vec<String> {
        let out = report.render_view(view);
        let (_, rows) = out.split_once("\n--\n").expect("the rows");
        rows.lines().map(str::to_string).collect()
    };

    let fixed = report_of(
        cx::END_OF_TURN,
        "Fix the bug in calc.py so add returns a + b. Edit the file with a patch; do not run \
         anything.",
    );
    assert_eq!(fixed.program, Program::Codex);
    assert!(fixed.complete(), "{}", fixed.header());
    assert_eq!(
        kept(&fixed, View::Messages),
        rows(&[
            "› Fix the bug in calc.py so add returns a + b. Edit the file with a patch; do not run \
             anything.",
            "",
            "• I’ll inspect calc.py and apply a patch only; I won’t run tests or commands after \
             editing.",
            "",
            "• Patched calc.py so add(a, b) returns a + b. No commands were run after editing.",
            "",
            "  1:38 PM",
            "",
            "› Write a 120-word paragraph about why tests matter. Do not use tools.",
            "",
            "• Tests matter because they turn assumptions into evidence. They reveal regressions \
             when new changes accidentally break",
            "  old behavior, and they document what the software is expected to do. A reliable \
             test suite gives developers confidence",
            "  to refactor, improve performance, and fix defects without fearing invisible \
             consequences. Tests also encourage clearer",
            "  design, since code that is difficult to test often has tangled responsibilities or \
             hidden dependencies. Fast, focused",
            "  checks provide immediate feedback, reducing the cost of finding mistakes late in \
             development. They support",
            "  collaboration by giving teams a shared, objective standard for correctness. Tests \
             cannot replace thoughtful review or",
            "  real-world monitoring, but together they create a safety net. Ultimately, testing \
             makes software more trustworthy,",
            "  maintainable, and resilient as it evolves continuously.",
            "",
            "  1:39 PM",
        ])
    );
    let final_rows = kept(&fixed, View::Final);
    assert_eq!(
        final_rows.first().map(String::as_str),
        Some(
            "• Tests matter because they turn assumptions into evidence. They reveal regressions when \
         new changes accidentally break"
        )
    );
    assert_eq!(final_rows.last().map(String::as_str), Some("  1:39 PM"));
    assert_eq!(final_rows.len(), 10, "{final_rows:#?}");

    let hi = report_of(
        cx::END_OF_TURN_0_163,
        "Reply with exactly the word hi and nothing else.",
    );
    assert_eq!(
        kept(&hi, View::Final),
        rows(&["• hi", "", "  Worked for 1s • 7:14 AM"])
    );

    let interrupted = report_of(
        cx::INTERRUPTED_0_163,
        "Run this shell command and nothing else: touch probe.txt",
    );
    assert_eq!(
        kept(&interrupted, View::Messages),
        rows(&[
            "› Run this shell command and nothing else: touch probe.txt",
            "",
            "■ Conversation interrupted - use /feedback if something went wrong",
        ])
    );
    assert!(
        kept(&interrupted, View::Final).is_empty(),
        "no message: nothing said"
    );

    // Negative control: Claude Code's grammar finds no `⏺` message and no
    // `❯` row in a Codex report.
    for report in [&fixed, &hi] {
        let claude = Report {
            program: Program::Claude,
            ..report.clone()
        };
        assert!(kept(&claude, View::Final).is_empty(), "{}", claude.header());
        assert!(
            kept(&claude, View::Messages).is_empty(),
            "{}",
            claude.header()
        );
    }
}

/// A CODEX worker on the ALTERNATE screen — 0.163.0's default (`modes`
/// `alt_screen=true`, measured in the lab on 2026-10-16) — is archived like
/// Claude Code, and its archive report is read in Codex's grammar: from the
/// ledger turn's `›` row (or the last one), its live zone cut (the composer
/// and footer; a running turn's status row), and `--final` / `--messages` in
/// its blocks (the `└ Tip:` under the answer is no word of it). The screen
/// names Codex, so no `status` is asked. In the lab, before, the same report
/// was read in Claude Code's grammar: no `❯` row (marker-not-found), the
/// whole screen with its composer and footer, and every view the same.
#[test]
fn a_codex_worker_on_the_alternate_screen_is_read_in_its_own_grammar() {
    use aterm_phase::codex::fixtures as cx;
    let screen = aterm_phase::prompt::fixtures::screen;
    let archived = |text: &str, turn: Option<&str>| {
        let read = Read::new(1, &[], screen(text));
        let history = match turn {
            Some(turn) => history(&[(42, true, "7730:100", turn)]),
            None => no_history(),
        };
        let mut host = Host::default()
            .on("history", history)
            .on("offscreen", read.ctl());
        let report = run(&mut host, ReportOpts::default());
        assert!(
            !host.requests.iter().any(|r| r.ends_with(" status")),
            "{:#?}",
            host.requests
        );
        report
    };
    let kept = |report: &Report, view: View| -> Vec<String> {
        let out = report.render_view(view);
        let (_, rows) = out.split_once("\n--\n").expect("the rows");
        rows.lines().map(str::to_string).collect()
    };

    let answered = archived(
        cx::QUESTION_ANSWERED_LAB_0_163,
        Some("QUESTION: plan the database migration"),
    );
    assert_eq!(
        answered.header(),
        "report complete=1 marker=ledger turn=42 rows=11 archived=0 screen=11 last=7730:0"
    );
    assert_eq!(answered.program, Program::Codex);
    assert_eq!(
        answered.rows.first().map(String::as_str),
        Some("› QUESTION: plan the database migration")
    );
    assert_eq!(
        answered.rows.last().map(String::as_str),
        Some("  Worked for 8s • 12:43 PM")
    );
    assert_eq!(
        kept(&answered, View::Final),
        rows(&[
            "• Hi! I am the local lab stub, not a model. (reply 4)",
            "",
            "  Worked for 8s • 12:43 PM",
        ])
    );
    assert_eq!(
        kept(&answered, View::Messages),
        rows(&[
            "› QUESTION: plan the database migration",
            "",
            "• Hi! I am the local lab stub, not a model. (reply 4)",
            "",
            "  Worked for 8s • 12:43 PM",
        ])
    );

    let esc = archived(cx::QUESTION_ESC_LAB_0_163, None);
    assert_eq!(esc.marker, Marker::UserRow);
    assert!(esc.complete(), "{}", esc.header());
    assert_eq!(
        kept(&esc, View::Messages),
        rows(&[
            "› QUESTION: plan the rollback",
            "",
            "■ Conversation interrupted - use /feedback if something went wrong",
        ])
    );

    let busy = archived(
        cx::BUSY_0_163,
        Some("Reply with exactly the word hi and nothing else."),
    );
    assert_eq!(
        busy.rows,
        rows(&["› Reply with exactly the word hi and nothing else."]),
        "{}",
        busy.header()
    );
}

/// A Codex reply TALLER than the screen, drawn in one frame on the alternate
/// screen (0.163.0 in the lab, 2026-10-16): the user's row stays pinned on
/// the screen's top row over the reply's last paragraphs, its first ones
/// never drawn, and the host's archive keeps the frame before it — the
/// user's row, the status row, the EMPTY composer and the footer (archived
/// rows 84 to 92, as read) — and a break. The report opens at the user's
/// row and says the gap (archive-gap); the archived composer is never the
/// start, and no view prints it or its footer as anybody's words. Before,
/// with no turn in the ledger the report opened at that composer (its last
/// `›` row) and `--messages` printed it and the footer as a person's.
#[test]
fn a_tall_codex_reply_opens_at_the_users_row_never_an_archived_composer() {
    use aterm_phase::codex::fixtures as cx;
    let archived = [
        "› Write sixty paragraphs.",
        "",
        "",
        "• Working (0s • esc to interrupt)",
        "",
        "",
        "› Ask Codex to do anything",
        "",
        "  GPT-6.1-Sol medium · /private/tmp/at.kbKP/proj · lab stub                                    \
                   Plan mode",
    ];
    let mut read = Read::new(
        84,
        &archived,
        aterm_phase::prompt::fixtures::screen(cx::TALL_REPLY_LAB_0_163),
    );
    read.breaks = 1;
    read.back = 1;
    read.back_at = Some(84);
    let mut host = Host::default()
        .on("history", no_history())
        .on("offscreen", read.ctl())
        .on("offscreen", recheck(1));
    let report = run(&mut host, ReportOpts::default());
    assert_eq!(report.marker, Marker::UserRow);
    assert_eq!(report.reasons, [Reason::ArchiveGap], "{}", report.header());
    assert_eq!(
        report.rows.first().map(String::as_str),
        Some("› Write sixty paragraphs.")
    );
    assert_eq!(
        report.rows.last().map(String::as_str),
        Some("  Worked for <1s • 12:50 PM")
    );
    assert!(
        report
            .rows
            .iter()
            .any(|r| r.trim() == "Paragraph 60 of a reply taller than the screen."),
        "{:#?}",
        report.rows
    );
    let out = report.render_view(View::Messages);
    let (_, kept) = out.split_once("\n--\n").expect("the rows");
    assert_eq!(
        kept, "› Write sixty paragraphs.\n\n  Worked for <1s • 12:50 PM\n",
        "{out}"
    );
}

/// A CODEX worker on the ALTERNATE screen whose screen names no program by
/// its layout — a composer holding a draft, with no turn running (measured
/// 0.156.1) — is named by `status`, asked once, and its archive report is
/// read in Codex's grammar: it opens at the ledger turn's `›` row and the
/// draft composer and footer are cut. Without the question, the screen's
/// silence read it in Claude Code's grammar: no `❯` row (marker-not-found)
/// and the draft kept as the report's last rows. NEGATIVE CONTROL: the same
/// screen in a session `status` names Claude Code is read in Claude Code's.
#[test]
fn a_codex_draft_on_the_alternate_screen_is_named_by_status() {
    use aterm_phase::codex::fixtures as cx;
    let draft = aterm_phase::prompt::fixtures::screen(cx::DRAFT);
    let report_as = |program: &str| {
        let mut host = Host::default()
            .on(
                "history",
                history(&[(
                    42,
                    true,
                    "7730:0",
                    "Run exactly this shell command: touch made-by-codex.txt",
                )]),
            )
            .on("offscreen", Read::new(1, &[], draft.clone()).ctl())
            .on("status", status(program));
        let report = run(&mut host, ReportOpts::default());
        let asked = host
            .requests
            .iter()
            .filter(|r| r.ends_with(" status"))
            .count();
        assert_eq!(asked, 1, "{:#?}", host.requests);
        report
    };
    let codex = report_as("codex");
    assert_eq!(codex.program, Program::Codex);
    assert_eq!(
        codex.header(),
        "report complete=1 marker=ledger turn=42 rows=13 archived=0 screen=13 last=7730:0"
    );
    assert_eq!(
        codex.rows.first().map(String::as_str),
        Some("› Run exactly this shell command: touch made-by-codex.txt")
    );
    assert_eq!(codex.rows.last().map(String::as_str), Some("  1:37 PM"));

    let claude = report_as("claude");
    assert_eq!(claude.program, Program::Claude);
    assert_eq!(claude.reasons, [Reason::MarkerNotFound]);
}

/// A Codex reply taller than the screen STREAMED on the alternate screen
/// (0.163.0 in the lab, 2026-10-16): Codex PINS the turn's prompt on the
/// screen's top row over paragraph 50, while the prompt's own row and
/// paragraphs 1 to 49 went to the archive (`back=0`: the host placed no row
/// of the screen against it). The report opens at the archived row and holds
/// the whole reply; `--final` is the reply from paragraph 1 to its end row,
/// `--messages` has one `›` row. Before, the pinned copy was the newest `›`
/// row: the report opened there, complete, without paragraphs 1 to 49, and
/// the views read paragraphs 50 to 70 as the person's message. NEGATIVE
/// CONTROL: a top row whose words are not the newest archived prompt's — a
/// person's row that is the top row — stays, and opens the report.
#[test]
fn a_codex_prompt_pinned_over_its_reply_is_no_start() {
    use aterm_phase::codex::fixtures as cx;
    let screen = aterm_phase::prompt::fixtures::screen(cx::PINNED_PROMPT_LAB_0_163);
    let paragraph = |n: u32| format!("Streamed paragraph {n} of a reply taller than the screen.");
    let mut archived = vec![
        "› Write seventy streamed paragraphs.".to_string(),
        String::new(),
    ];
    for n in 1..=49 {
        let lead = if n == 1 { "• " } else { "  " };
        archived.push(format!("{lead}{}", paragraph(n)));
        archived.push(String::new());
    }
    let archived: Vec<&str> = archived.iter().map(String::as_str).collect();
    let report_of = |screen: Vec<String>| {
        let mut host = Host::default()
            .on("history", no_history())
            .on("offscreen", Read::new(70, &archived, screen).ctl());
        run(&mut host, ReportOpts::default())
    };
    let kept = |report: &Report, view: View| -> Vec<String> {
        let out = report.render_view(view);
        let (_, rows) = out.split_once("\n--\n").expect("the rows");
        rows.lines().map(str::to_string).collect()
    };

    let report = report_of(screen.clone());
    assert!(report.complete(), "{}", report.header());
    assert_eq!(
        report.rows.first().map(String::as_str),
        Some("› Write seventy streamed paragraphs.")
    );
    assert_eq!(report.archived, 100, "{}", report.header());
    assert_eq!(
        report.rows.iter().filter(|r| r.starts_with('›')).count(),
        1,
        "{:#?}",
        report.rows
    );
    let last = kept(&report, View::Final);
    assert_eq!(
        last.first().map(String::as_str),
        Some(format!("• {}", paragraph(1)).as_str())
    );
    assert_eq!(
        last.last().map(String::as_str),
        Some("  Worked for 9s • 1:41 PM")
    );
    assert!(last.iter().any(|r| r.trim() == paragraph(70)), "{last:#?}");
    let said = kept(&report, View::Messages);
    assert_eq!(said[0], "› Write seventy streamed paragraphs.");
    assert_eq!(said[2], format!("• {}", paragraph(1)));

    // The control: another prompt on the top row is a person's row.
    let mut other = screen;
    other[0] = "› Write seventy more paragraphs.".to_string();
    let report = report_of(other);
    assert_eq!(
        report.rows.first().map(String::as_str),
        Some("› Write seventy more paragraphs.")
    );
    assert_eq!(report.archived, 0, "{}", report.header());
}
