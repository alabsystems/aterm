// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-link ls` PAGES A `Last` FACE ON THE RESUME CURSOR — the rule the rest
//! of the crate already keeps, proved against the one reader that did not.
//!
//! §5.2 and [`astream_broker::Client::last_page`]'s own contract: the broker
//! clamps `max` AND bounds how many index entries one request may VISIT, matched
//! or not, so a page shorter than `max` — AN EMPTY ONE INCLUDED — is not the end
//! of the answer. Only an empty `resume` is. `roster()` used to call
//! `Client::last`, which throws the cursor away, and stop on `rows.is_empty()`.
//!
//! A LIVE BROKER CANNOT SHOW THIS TODAY, which is the whole reason it survived
//! two audit rounds: `store::latest_matching` only surrenders a resume cursor on
//! a subject it has already pushed, so today's empty page happens to coincide
//! with a finished walk. The defect is that the reader depends on that accident
//! rather than on the contract — so the contract is what is tested here, with a
//! stub that answers `Last` exactly as the contract permits and the real
//! `aterm-link ls` binary on the other end of the socket.
//!
//! DETERMINISTIC: no sleeps, no live broker, no clock. The stub answers in the
//! request thread and the assertions are on the binary's stdout and exit code.

use std::io::Read;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::Command;

use astream_broker::proto::{
    decode_request, encode_response, read_frame, write_frame, Request, Response,
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
        };
        if write_frame(&mut s, &encode_response(&mark)).is_err() {
            return;
        }
    }
}

/// Run the real `aterm-link ls` against a stub broker speaking `pages`.
/// Answers `(stdout, stderr, success)`.
fn ls(tag: &str, pages: Vec<Page>, extra: &[&str]) -> (String, String, bool) {
    let path = sock_path(tag);
    let listener = UnixListener::bind(&path).expect("bind the stub broker");
    let server = std::thread::spawn(move || {
        if let Ok((s, _)) = listener.accept() {
            answer(s, &pages);
        }
    });
    let out = Command::new(env!("CARGO_BIN_EXE_aterm-link"))
        .arg("ls")
        .args(["--fleet", "f1", "--broker"])
        .arg(&path)
        .args(extra)
        .output()
        .expect("run aterm-link ls");
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

/// AN EMPTY PAGE IS NOT THE END OF THE ROSTER, and neither is a short one.
///
/// The script is the contract's worst legal shape: a page with a row and a
/// cursor, then a page with NO rows and a cursor, then the last page. A reader
/// that stops on `rows.is_empty()` prints `n-a` and exits 0 — a fleet of three
/// nodes reported as a fleet of one, with no error and no marker.
#[test]
fn ls_pages_past_an_empty_page_because_only_an_empty_resume_ends_the_walk() {
    let (stdout, stderr, ok) = ls(
        "pg",
        vec![
            Page::row(
                1,
                "n-a",
                "s-1",
                "v=1 t=1 state=live",
                "/f/f1/pub/n-a/s-1/presence",
            ),
            Page::empty("/f/f1/pub/n-b/s-2/presence"),
            Page::row(3, "n-c", "s-3", "v=1 t=3 state=live", ""),
        ],
        &[],
    );
    assert!(ok, "ls failed: {stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "the whole roster, not the first page: {stdout}"
    );
    assert!(lines[0].starts_with("n-a "), "{stdout}");
    assert!(
        lines[1].starts_with("n-c "),
        "the row BEHIND the empty page is the one a truncating reader loses: {stdout}"
    );
}

/// §9.3's ESCALATION VIEW is the case that matters: the node raising
/// `attention=` sits behind the empty page, so a reader that stops there prints
/// nothing and exits 0 — silence indistinguishable from "nobody needs you".
#[test]
fn ls_attention_sees_an_escalation_that_sits_behind_an_empty_page() {
    let (stdout, stderr, ok) = ls(
        "att",
        vec![
            Page::empty("/f/f1/pub/n-a/s-1/presence"),
            Page::row(2, "n-b", "s-2", "v=1 t=2 attention=needs-you", ""),
        ],
        &["--attention"],
    );
    assert!(ok, "ls --attention failed: {stderr}");
    assert!(
        stdout.contains("attention=needs-you"),
        "the escalation behind the empty page must be printed: {stdout:?}"
    );
}

/// A WALK THAT COULD NOT FINISH IS AN ERROR, NOT AN EMPTY FLEET.
///
/// The stub never surrenders an empty `resume`, so the walk is unbounded. `ls`
/// must stop at the page bound and SAY SO on stderr with a non-zero exit — a
/// caller that cannot tell "no rows" from "I stopped looking" is the caller that
/// reports a quiet fleet while a node is escalating.
#[test]
fn a_last_walk_that_never_ends_is_an_error_rather_than_a_short_roster() {
    let (stdout, stderr, ok) = ls("bnd", vec![Page::empty("/f/f1/pub/n-a/s-1/presence")], &[]);
    assert!(!ok, "an unfinished walk must not exit 0: {stdout:?}");
    assert!(
        stderr.contains("did not finish"),
        "the failure must name itself: {stderr:?}"
    );
}

/// The stub is only worth what its framing is: prove one `Last` request really
/// reaches it and really decodes, so a green suite above cannot be a binary that
/// failed to connect.
#[test]
fn the_stub_broker_really_speaks_the_last_verb() {
    let path = sock_path("frm");
    let listener = UnixListener::bind(&path).expect("bind");
    let seen = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("accept");
        let frame = read_frame(&mut s).expect("read").expect("a frame");
        let req = decode_request(&frame);
        let mark = Response::Mark {
            next: 0,
            head: 0,
            resume: String::new(),
        };
        let _ = write_frame(&mut s, &encode_response(&mark));
        let mut sink = Vec::new();
        let _ = s.read_to_end(&mut sink);
        req
    });
    let out = Command::new(env!("CARGO_BIN_EXE_aterm-link"))
        .arg("ls")
        .args(["--fleet", "f1", "--broker"])
        .arg(&path)
        .output()
        .expect("run");
    // The same total join as `ls`: a client that never connected must fail the
    // assertions below, not park this test in `accept()`.
    let _ = UnixStream::connect(&path);
    let req = seen.join().expect("stub thread");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    match req {
        Some(Request::Last { filter, after, max }) => {
            assert_eq!(filter, "/f/f1/pub/*/*/presence");
            assert_eq!(after, "", "the first page starts at the beginning");
            assert_eq!(max, aterm_link::transport::LAST_PAGE_ROWS);
        }
        other => panic!("ls must open with a Last: {other:?}"),
    }
}
