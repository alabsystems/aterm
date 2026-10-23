// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm link ls` over a scripted `Last` response.
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

struct Page {
    rows: Vec<(u64, String, Vec<u8>)>,
    resume: String,
}

impl Page {
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

    fn empty(resume: &str) -> Self {
        Self {
            rows: Vec::new(),
            resume: resume.to_string(),
        }
    }
}

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

fn sock_path(tag: &str) -> (aterm_tempfile::TempDir, PathBuf) {
    let dir = aterm_tempfile::Builder::new()
        .prefix("atl-")
        .tempdir_in("/tmp")
        .expect("a scratch dir under /tmp");
    let p = dir.path().join(format!("{tag}.sock"));
    assert!(
        p.as_os_str().len() < 100,
        "socket path too long for sun_path: {}",
        p.display()
    );
    (dir, p)
}

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

fn ls(tag: &str, pages: Vec<Page>, extra: &[&str]) -> (String, String, bool) {
    let (_dir, path) = sock_path(tag);
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
    let _ = UnixStream::connect(&path);
    server.join().expect("the stub broker thread");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

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

#[test]
fn a_last_walk_that_never_ends_is_an_error_rather_than_a_short_roster() {
    let (stdout, stderr, ok) = ls("bnd", vec![Page::empty("/f/f1/pub/n-a/s-1/presence")], &[]);
    assert!(!ok, "an unfinished walk must not exit 0: {stdout:?}");
    assert!(stdout.is_empty(), "no partial roster: {stdout:?}\n{stderr}");
}
