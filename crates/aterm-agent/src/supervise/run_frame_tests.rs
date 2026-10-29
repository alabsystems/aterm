// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervisor whole-frame read tests ([`Session::text_json`]).

use super::*;
use aterm_phase::prompt::fixtures::screen as fx;
use std::collections::VecDeque;

const CLAUDE_HALF: &str =
    include_str!("../../../aterm-phase/src/fixtures/claude-2.1.308-lab-split-half.txt");
const CLAUDE_WHOLE: &str =
    include_str!("../../../aterm-phase/src/fixtures/claude-2.1.308-lab-split-whole.txt");
const AGY_HALF: &str =
    include_str!("../../../aterm-phase/src/fixtures/agy-1.4.0-lab-split-half.txt");
const AGY_WHOLE: &str =
    include_str!("../../../aterm-phase/src/fixtures/agy-1.4.0-lab-split-whole.txt");

/// A host serving `text --json` reads (whole or `tail=`) from a script.
struct Host {
    reads: VecDeque<(Vec<String>, bool)>,
    knows_frame: bool,
    /// The `"first"` a `tail=` read says its rows start at.
    tail_first: Option<usize>,
    requests: Vec<String>,
    seq: u64,
}

impl Host {
    fn new(reads: Vec<(&str, bool)>) -> Self {
        Host {
            reads: reads.into_iter().map(|(t, open)| (fx(t), open)).collect(),
            knows_frame: true,
            tail_first: None,
            requests: Vec::new(),
            seq: 100,
        }
    }

    fn read(&mut self, tail: bool) -> String {
        let (rows, open) = if self.reads.len() > 1 {
            self.reads.pop_front().expect("a read")
        } else {
            self.reads.front().cloned().expect("a read")
        };
        self.seq += 1;
        let rows: Vec<String> = rows
            .iter()
            .map(|r| format!("\"{}\"", r.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect();
        let mut frame = if open { r#","frame":"open""# } else { "" }.to_string();
        if let Some(first) = self.tail_first.filter(|_| tail) {
            frame.push_str(&format!(",\"first\":{first}"));
        }
        format!(
            "OK 1\n{{\"rows\":[{}],\"cursor\":{{\"row\":46,\"col\":2,\"visible\":true}},\
             \"dims\":{{\"rows\":{},\"cols\":144}},\"seq\":{},\"submits\":1{frame}}}\n",
            rows.join(","),
            rows.len(),
            self.seq
        )
    }
}

impl Ctl for Host {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        self.requests.push(args.join(" "));
        let reply = |code: i32, stdout: String, stderr: &str| CtlReply {
            code,
            stdout,
            stderr: stderr.to_string(),
        };
        Ok(match args {
            ["text", "--json"] => reply(0, self.read(false), ""),
            ["text", "--json", tail] if tail.starts_with("tail=") => reply(0, self.read(true), ""),
            ["await", "frame", "timeout", _] if self.knows_frame => {
                reply(0, format!("OK frame {}\n", self.seq), "")
            }
            ["await", "frame", ..] => reply(
                1,
                String::new(),
                "aterm-ctl: ERR usage: await <idle <ms>|seq [<n>]|match <re>|gone <re>|block>\n",
            ),
            _ => return Err(format!("unscripted request {args:?}")),
        })
    }
}

fn claude() -> &'static dyn aterm_phase::ScreenReader {
    aterm_phase::Program::Claude.reader()
}

fn agy() -> &'static dyn aterm_phase::ScreenReader {
    aterm_phase::Program::Agy.reader()
}

#[test]
fn a_read_of_an_open_frame_waits_for_the_frame_and_reads_again() {
    for (name, half, whole, reader) in [
        ("claude", CLAUDE_HALF, CLAUDE_WHOLE, claude()),
        ("agy", AGY_HALF, AGY_WHOLE, agy()),
    ] {
        let mut host = Host::new(vec![(half, true), (whole, false)]);
        let mut s = Session::new(&mut host, None);
        let screen = s.grid().expect("a read");
        assert!(!screen.frame_open, "{name}");
        assert_eq!(
            screen.rows,
            fx(whole),
            "{name}: the whole frame is the read"
        );
        assert_eq!(reader.phase(&screen.rows), Phase::Busy, "{name}");
        drop(s);
        assert_eq!(
            host.requests,
            ["text --json", "await frame timeout 61000", "text --json"],
            "{name}"
        );
    }
}

/// NEGATIVE CONTROL: a whole frame is read once, with no wait.
#[test]
fn a_whole_read_is_read_once() {
    let mut host = Host::new(vec![(CLAUDE_WHOLE, false)]);
    let mut s = Session::new(&mut host, None);
    let screen = s.grid().expect("a read");
    assert_eq!(screen.rows, fx(CLAUDE_WHOLE));
    drop(s);
    assert_eq!(host.requests, ["text --json"]);
}

/// MUTANT, and the older host: the half read with no `"frame"` from the server is taken as it
/// stands, which its reader judges idle mid-reply: the defect the field and the wait close.
#[test]
fn a_half_read_without_the_servers_word_is_judged_as_it_stands() {
    for (half, reader) in [(CLAUDE_HALF, claude()), (AGY_HALF, agy())] {
        let mut host = Host::new(vec![(half, false), (CLAUDE_WHOLE, false)]);
        let mut s = Session::new(&mut host, None);
        let screen = s.grid().expect("a read");
        assert_eq!(reader.phase(&screen.rows), Phase::Idle, "the defect");
        drop(s);
        assert_eq!(host.requests, ["text --json"]);
    }
}

/// A host that says `"frame":"open"` but refuses `await frame` (none ships so; a wire
/// mismatch): the read is taken as it stands after the one refused wait, never a fault.
#[test]
fn a_host_without_await_frame_reads_as_it_stands() {
    let mut host = Host::new(vec![(CLAUDE_HALF, true), (CLAUDE_WHOLE, false)]);
    host.knows_frame = false;
    let mut s = Session::new(&mut host, None);
    let screen = s.grid().expect("a read");
    assert!(screen.frame_open);
    drop(s);
    assert_eq!(host.requests, ["text --json", "await frame timeout 61000"]);
}

/// A frame open at every read is read again [`FRAME_REREADS`] times, then taken as it stands:
/// the reads are bounded, the server's hang detector bounds each wait.
#[test]
fn rereads_of_frames_open_at_every_read_are_bounded() {
    let mut host = Host::new(vec![(CLAUDE_HALF, true)]);
    let mut s = Session::new(&mut host, None);
    let screen = s.grid().expect("a read");
    assert!(screen.frame_open);
    drop(s);
    let reads = host.requests.iter().filter(|r| *r == "text --json").count();
    let waits = host
        .requests
        .iter()
        .filter(|r| r.starts_with("await frame"))
        .count();
    assert_eq!(reads, FRAME_REREADS as usize + 1);
    assert_eq!(waits, FRAME_REREADS as usize);
}

/// THE LOOP'S OWN READ IS WHOLE FRAMES TOO ([`Session::read_shaped`], every read of the turn's
/// wait): its `tail=` read caught open is read again after `await frame`, and so is the whole read
/// a tail that cannot stand is taken again as. MUTANT: either read made raw (a bare `call` and
/// [`parse_text_json`]) hands the loop the half frame, idle to its reader.
#[test]
fn the_loops_tail_and_whole_reads_wait_for_the_frame() {
    let mut host = Host::new(vec![(CLAUDE_HALF, true), (CLAUDE_WHOLE, false)]);
    let mut s = Session::new(&mut host, None);
    let read = s.read_shaped().expect("a read");
    assert!(read.above.is_none(), "the tail stood");
    assert!(!read.screen.frame_open);
    assert_eq!(read.screen.rows, fx(CLAUDE_WHOLE));
    assert_eq!(claude().phase(&read.screen.rows), Phase::Busy);
    drop(s);
    assert_eq!(
        host.requests,
        [
            "text --json tail=40",
            "await frame timeout 61000",
            "text --json tail=40"
        ]
    );

    let mut host = Host::new(vec![
        (CLAUDE_WHOLE, false),
        (CLAUDE_HALF, true),
        (CLAUDE_WHOLE, false),
    ]);
    host.tail_first = Some(47);
    let mut s = Session::new(&mut host, None);
    let read = s.read_shaped().expect("a read");
    assert!(read.above.is_some(), "the read was whole");
    assert!(!read.screen.frame_open);
    assert_eq!(claude().phase(&read.screen.rows), Phase::Busy);
    drop(s);
    assert_eq!(
        host.requests,
        [
            "text --json tail=40",
            "text --json",
            "await frame timeout 61000",
            "text --json"
        ]
    );
}
