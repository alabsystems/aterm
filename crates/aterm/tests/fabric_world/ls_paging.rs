// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm link ls` OVER A SCRIPTED `Last`: what `--attention` keeps, and that
//! a walk which could not finish is a failure.
//!
//! The paging itself is astream's: `ls` walks the face with
//! `astream_broker::Client::last_walk`, which pages on the resume cursor —
//! through short and EMPTY pages, ending only on an empty `resume`, and failing
//! past `LAST_WALK_PAGES_MAX` — and astream's own `tests/last_walk.rs` proves
//! that contract. A live broker rarely shows an empty page with a live cursor
//! (its `last_page` hands back a cursor only on a subject the filter covers),
//! so the stub here answers `Last` in the contract's worst legal shapes, with
//! the real `aterm link ls` on the other end of the socket, to check the parts
//! that are aterm's: the filter over the rows and the exit status.
//!
//! DETERMINISTIC: no sleeps, no live broker, no clock.
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use crate::harness;

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::Command;

use astream_broker::proto::{
    Request, Response, decode_request, encode_response, read_frame, write_frame,
};

/// One scripted `Last` answer: the rows the page carries and the cursor that
/// closes it. An empty `resume` says the walk is over.
struct Page {
    rows: Vec<(u64, String, Vec<u8>)>,
    resume: String,
}

impl Page {
    /// A page carrying one presence row.
    fn row(offset: u64, node: &str, sid: &str, body: &str, resume: &str) -> Self {
        Self {
            rows: vec![(
                offset,
                format!("/f/f1/pub/{node}/{sid}/presence"),
                body.as_bytes().to_vec(),
            )],
            resume: resume.to_string(),
        }
    }

    /// A page carrying NO rows and a cursor — the shape the contract permits and
    /// the shape the old reader read as "the fleet ends here".
    fn empty(resume: &str) -> Self {
        Self {
            rows: Vec::new(),
            resume: resume.to_string(),
        }
    }
}

/// `aterm link` with no rendezvous file to read: the child's `XDG_RUNTIME_DIR`
/// is an empty scratch dir, so `ls` gets exactly the flags a test spells out.
/// Against the real one, a machine where `aterm fabric on` had written it
/// filled in its `--cap-file`, and `ls` opened with the `Hello` an attach
/// needs, which these stubs (they speak `Last` only) hang up on — every test
/// here red on 2026-09-30 for the machine, not the tree. The dir goes when the
/// returned guard drops: hold it until the child has exited.
fn aterm_link() -> (Command, aterm_tempfile::TempDir) {
    let runtime = aterm_tempfile::Builder::new()
        .prefix("atl-rt-")
        .tempdir()
        .expect("a scratch XDG_RUNTIME_DIR");
    let mut c = Command::new(harness::aterm_to_launch());
    c.arg("link");
    c.env("XDG_RUNTIME_DIR", runtime.path());
    (c, runtime)
}

/// A socket path short enough for `sockaddr_un.sun_path` on macOS.
fn sock_path(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("atl-{tag}-{}.sock", std::process::id()));
    assert!(
        p.as_os_str().len() < 100,
        "socket path too long for sun_path: {}",
        p.display()
    );
    let _ = std::fs::remove_file(&p);
    p
}

/// Answer every `Last` on one connection from `pages`, in order, repeating the
/// LAST page once the script runs out — which is how the unbounded-walk case is
/// scripted without a special stub.
fn answer(mut s: UnixStream, pages: &[Page]) {
    let mut i = 0usize;
    while let Ok(Some(frame)) = read_frame(&mut s) {
        let Some(Request::Last { .. }) = decode_request(&frame) else {
            return;
        };
        let page = &pages[i.min(pages.len() - 1)];
        i += 1;
        for (offset, subject, body) in &page.rows {
            let resp = Response::Delivery {
                offset: *offset,
                subject: subject.clone(),
                body: body.clone(),
            };
            if write_frame(&mut s, &encode_response(&resp)).is_err() {
                return;
            }
        }
        let mark = Response::Mark {
            next: 0,
            head: 0,
            resume: page.resume.clone(),
            durable: None,
            oldest: None,
        };
        if write_frame(&mut s, &encode_response(&mark)).is_err() {
            return;
        }
    }
}

/// Run the real `aterm link ls` against a stub broker speaking `pages`.
/// Answers `(stdout, stderr, success)`.
fn ls(tag: &str, pages: Vec<Page>, extra: &[&str]) -> (String, String, bool) {
    let path = sock_path(tag);
    let listener = UnixListener::bind(&path).expect("bind the stub broker");
    let server = std::thread::spawn(move || {
        if let Ok((s, _)) = listener.accept() {
            answer(s, &pages);
        }
    });
    let (mut cmd, _runtime) = aterm_link();
    cmd.arg("ls")
        .args(["--fleet", "f1", "--broker"])
        .arg(&path)
        .args(extra);
    let out = harness::output_within(&mut cmd, "aterm link ls");
    // UNBLOCK THE ACCEPT rather than time it out. If `ls` exited without ever
    // connecting — a usage error, a parse change — the stub would sit in
    // `accept()` for ever and the test would HANG instead of failing. One
    // throwaway connection makes the join total; it is queued and ignored when
    // `ls` did connect.
    let _ = UnixStream::connect(&path);
    server.join().expect("the stub broker thread");
    let _ = std::fs::remove_file(&path);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// `--attention` (§9.3's ESCALATION VIEW) PRINTS THE ESCALATING ROWS AND ONLY
/// THEM, wherever the walk finds them: a quiet row first, then an empty page
/// with a live cursor, then the node raising `attention=`. A filter that kept
/// everything prints two lines; a walk that stopped at the empty page prints
/// nothing and exits 0 — silence indistinguishable from "nobody needs you".
#[test]
fn ls_attention_prints_only_the_escalating_rows_past_an_empty_page() {
    let (stdout, stderr, ok) = ls(
        "att",
        vec![
            Page::row(
                1,
                "n-a",
                "s-1",
                "v=1 t=1 state=live",
                "/f/f1/pub/n-a/s-1/presence",
            ),
            Page::empty("/f/f1/pub/n-a/s-9/presence"),
            Page::row(3, "n-b", "s-2", "v=1 t=2 attention=needs-you", ""),
        ],
        &["--attention"],
    );
    assert!(ok, "ls --attention failed: {stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "only the escalation: {stdout:?}");
    assert!(
        lines[0].starts_with("n-b ") && lines[0].contains("attention=needs-you"),
        "the escalation behind the empty page must be printed: {stdout:?}"
    );
}

/// A WALK THAT COULD NOT FINISH IS AN ERROR, NOT AN EMPTY FLEET.
///
/// The stub never surrenders an empty `resume`, so the walk is unbounded and
/// astream's walker gives up at its page bound. `ls` must hand that error on —
/// with a non-zero exit — rather than swallow it: a caller that cannot tell
/// "no rows" from "I stopped looking" is the caller that reports a quiet fleet
/// while a node is escalating.
#[test]
fn a_last_walk_that_never_ends_is_an_error_rather_than_a_short_roster() {
    let (stdout, stderr, ok) = ls("bnd", vec![Page::empty("/f/f1/pub/n-a/s-1/presence")], &[]);
    assert!(!ok, "an unfinished walk must not exit 0: {stdout:?}");
    assert!(stdout.is_empty(), "no partial roster: {stdout:?}\n{stderr}");
}
