// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `report`, over a scripted host: the joins, the start, every reason, and the
//! requests it makes. Screens are synthetic, shaped like Claude Code's.

use std::collections::{BTreeMap, VecDeque};

use super::super::blocks::View;
use super::super::phase::transcript_end;
use super::super::prompt::fixtures::{composer, rows};
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
        self.replies
            .get_mut(verb)
            .and_then(VecDeque::pop_front)
            .ok_or_else(|| format!("unscripted request: {}", args.join(" ")))
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

/// The protocol's escape, for building `history` lines.
fn pct(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_graphic() && b != b'%' {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// `history` as `aterm ctl` prints it: the records on stdout.
fn history(turns: &[(u64, bool, &str, &str)]) -> CtlReply {
    let mut out = String::new();
    for (id, submitted, arch, text) in turns {
        out.push_str(&format!(
            "turn {id} submitted={} status=settled started_ms=1 dur_ms=2 seq=3 \
             hash=0000000000000000 arch={arch} text={}\n",
            u8::from(*submitted),
            pct(text)
        ));
    }
    ok(&out)
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
/// matches: the report opens at the LAST such row (the one above it on the
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
            .on("text", ok(&json));
        let report = run(&mut host, ReportOpts::default());
        assert_eq!(host.requests[2], "@s-1 text --json", "the full screen");
        assert_eq!(
            report.header(),
            "report complete=0 reason=no-archive marker=ledger turn=2 rows=2 archived=0 \
             screen=2 last=-"
        );
        assert_eq!(report.rows, rows(&["❯ Keep going", "⏺ Kept going."]));
    }
    // A host that has neither `history` nor `offscreen`.
    let mut host = Host::default()
        .on("history", err("unknown verb (try: help)"))
        .on("offscreen", err("unknown verb (try: help)"))
        .on("text", ok(&json));
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
/// a worker off the alternate screen is main-screen.
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

    let mut read = Read::new(1, &[], rows(&["$ codex", "❯ Keep going", "ok", "$"]));
    read.alt = false;
    let mut host = Host::default()
        .on("history", history(&[(2, true, "7730:0", "Keep going")]))
        .on("offscreen", read.ctl());
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

// -------------------------------------------- round-7 review of the report

/// The `[Pasted text` fallback fired for ANY ledger text that was not found —
/// here a short turn queued under the status row — and with an earlier turn's
/// paste on the screen the report claimed `complete=1 marker=ledger turn=8`
/// while it opened at turn 7's paste. A short turn is never a paste.
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
/// worker's numbered question, typed but not sent) was skipped by
/// `composer_index` as an option row, so no composer frame was found and the
/// report was the draft, the rule and the footer. The caret in column 0 under
/// a composer rule is the composer, whatever it holds.
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

/// Above an idle composer (no done row), the worker's last transcript row
/// was dropped when it was indented 20+ columns — indented code — taken for a
/// hint. A hint is parked against the right edge; code is not.
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

/// DRIVE_HELP promises "every other row is kept verbatim (done rows, …)", but
/// the done row that ends an idle turn — here the one that says a monitor is
/// STILL RUNNING — was taken for the status row and cut.
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
/// STATUS line: it prints `OK <n> first=…` on stdout and drops the rows. That
/// was a hard failure (`the header counts 12 rows, 0 came`); DRIVE_HELP
/// promises no-archive and the screen alone.
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
        .on("text", ok(&json));
    let report = Session::new(&mut host, Some("@s-1".to_string())).report(&ReportOpts::default());
    assert!(
        report
            .as_ref()
            .is_ok_and(|r| r.reasons.contains(&Reason::NoArchive)),
        "{report:?}"
    );
}

/// A host whose archive is OFF (`enabled=0`) kept nothing that scrolled away:
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

// ------------------------------------------- round-11: the report's two views

/// The `--final` and `--messages` views on the saved screen of a REAL worker
/// (`idle-after-limit-and-model-switch`, the fixture
/// [`the_transcript_ends_at_the_live_zone`] reads): the default report is
/// byte-identical to what it always printed, `--final` is the worker's last
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
