// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The fixture tapes in `tapes/` (design §21.3): each was recorded by hand from live,
//! read-only, anonymous calls to the public `alabsystems/aterm`. Each parses, holds nothing
//! but the public repository's own answers, and replays offline to the output it recorded.

use std::sync::Arc;

use agh::fx::tape::{self, TapeEndpoints, TapeMode};
use agh::fx::{DOOR_EXIT, DOOR_GH, DOOR_STDOUT};
use agh::redact::Redactor;
use agh::wire::Endpoints;
use agh::wire::scripted::ScriptedTransport;
use astream_effects::EffectRecord;

use crate::common::{self, run_cli, scratch};

/// Each tape's name and text. `release-list` is not here: its first page's body is over the
/// 1 MiB a tape holds, so it is taped by digest only and cannot replay.
const FIXTURES: &[(&str, &str)] = &[
    ("repo-view", include_str!("tapes/repo-view.atape")),
    ("release-latest", include_str!("tapes/release-latest.atape")),
    ("release-view", include_str!("tapes/release-view.atape")),
    ("cat-readme", include_str!("tapes/cat-readme.atape")),
    ("cat-missing", include_str!("tapes/cat-missing.atape")),
    ("ls-root", include_str!("tapes/ls-root.atape")),
    ("log", include_str!("tapes/log.atape")),
    ("rate-anonymous", include_str!("tapes/rate-anonymous.atape")),
    (
        "api-default-branch",
        include_str!("tapes/api-default-branch.atape"),
    ),
];

/// The private development owner's name, in halves as `aterm-release`'s transforms spell it,
/// so neither the export's rewrite nor its guard touches this file.
const PRIVATE_ORG: &str = concat!("andrew", "yatesai");

/// The developer account word, in halves for the same reason.
const ACCOUNT: &str = concat!("aya", "tes");

/// The one repository a fixture may name.
const CHANNEL: &str = "alabsystems/aterm";

/// The parsed fixture `name`.
fn parsed(name: &str, text: &str) -> (tape::TapeHeader, Vec<EffectRecord>) {
    tape::parse(text).unwrap_or_else(|e| panic!("{name}: {e:?}"))
}

/// Each `gh` exchange's request, as its canonical lines.
fn requests(records: &[EffectRecord]) -> Vec<Vec<String>> {
    records
        .iter()
        .filter_map(|record| match record {
            EffectRecord::Exchange { door, request, .. } if door == DOOR_GH => Some(
                String::from_utf8(request.clone())
                    .expect("a UTF-8 request")
                    .lines()
                    .map(str::to_string)
                    .collect(),
            ),
            _ => None,
        })
        .collect()
}

/// The concatenated bytes of every emit at `door`.
fn emitted(records: &[EffectRecord], door: &str) -> Vec<u8> {
    records
        .iter()
        .filter_map(|record| match record {
            EffectRecord::Emit { door: d, bytes } if d == door => Some(bytes.as_slice()),
            _ => None,
        })
        .flatten()
        .copied()
        .collect()
}

#[test]
fn tape_fixtures_are_public() {
    for (name, text) in FIXTURES {
        let (header, records) = parsed(name, text);
        assert_eq!(header.mode, TapeMode::Anonymous, "{name}");
        assert_eq!(header.endpoints, TapeEndpoints::GithubCom, "{name}");

        let requests = requests(&records);
        assert!(!requests.is_empty(), "{name}: no request");
        for lines in &requests {
            let head = lines.first().expect("a request line");
            assert!(
                lines.iter().any(|l| l == "origin api" || l == "origin web"),
                "{name}: {lines:?}"
            );
            assert!(
                lines.iter().any(|l| l == "as anonymous"),
                "{name}: {lines:?}"
            );
            let path = head.split_once(' ').map_or("", |(_, p)| p);
            if let Some(rest) = path.strip_prefix("/repos/") {
                let slug: Vec<&str> = rest.split(['/', '?']).take(2).collect();
                assert_eq!(slug.join("/"), CHANNEL, "{name}: {head}");
            } else {
                assert!(path.starts_with("/rate_limit"), "{name}: {head}");
            }
        }

        assert_eq!(Redactor::shapes(text), *text, "{name}: a token-shaped run");
        for line in text.lines() {
            let lower = line.to_ascii_lowercase();
            assert!(!lower.contains("authorization"), "{name}: {line}");
            assert!(!line.contains("/Users/"), "{name}: a home path");
            assert!(!lower.contains(PRIVATE_ORG), "{name}: the private owner");
            assert!(!lower.contains(ACCOUNT), "{name}: the account word");
        }
    }
}

#[test]
fn tape_fixtures_replay_offline() {
    for (name, text) in FIXTURES {
        let (_, records) = parsed(name, text);
        let exit: u8 = String::from_utf8(emitted(&records, DOOR_EXIT))
            .expect("a UTF-8 exit")
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("{name}: an exit emit: {e}"));
        let stdout = emitted(&records, DOOR_STDOUT);

        let root = scratch(&format!("fixture-{name}"));
        let path = root.path().join(format!("{name}.atape"));
        std::fs::write(&path, text).expect("write the tape");
        let transport = Arc::new(ScriptedTransport::new());
        let (mut ctx, captured) = agh::cli::Ctx::for_tests(
            root.path(),
            transport.clone(),
            Endpoints::github_com(),
            common::env(),
        );
        let path = path.display().to_string();
        let ran = run_cli(&mut ctx, &["--replay", &path]);

        let stderr = captured.stderr_text();
        assert_eq!(ran, exit, "{name}: stderr {stderr}");
        assert_eq!(captured.stdout_text().as_bytes(), stdout, "{name}");
        assert!(
            transport.requests().is_empty(),
            "{name}: a request reached the transport"
        );
        assert!(!stderr.contains("replay."), "{name}: {stderr}");
    }
}

// A bare `--replay` of a tape that can be read once (a pipe, as `cat T | agh --replay
// /dev/stdin` hands one over): `cli::run` reads it for its command line and hands the same
// tape to the run, so the replay never opens the pipe again, which would read it empty and
// refuse the tape. The pipe is this process's own (`std::io::pipe`), named by `/dev/fd/N`,
// so no child is spawned (plan §1, rule 4); the hang detector bounds a read that never ends.
#[cfg(unix)]
#[test]
fn a_fixture_replays_from_a_pipe_with_a_bare_replay() {
    use std::io::Write as _;
    use std::os::fd::AsRawFd as _;
    use std::sync::mpsc;

    let (name, text) = FIXTURES[0];
    let (_, records) = parsed(name, text);
    let stdout = emitted(&records, DOOR_STDOUT);
    let root = scratch("fixture-pipe");
    let (reader, mut writer) = std::io::pipe().expect("a pipe");
    std::thread::spawn(move || {
        // One writer, once: its drop is the reader's EOF.
        writer.write_all(text.as_bytes()).expect("the tape");
    });
    let (tx, rx) = mpsc::channel();
    let dir = root.path().to_path_buf();
    let path = format!("/dev/fd/{}", reader.as_raw_fd());
    std::thread::spawn(move || {
        let transport = Arc::new(ScriptedTransport::new());
        let (mut ctx, captured) = agh::cli::Ctx::for_tests(
            &dir,
            transport.clone(),
            Endpoints::github_com(),
            common::env(),
        );
        let ran = run_cli(&mut ctx, &["--replay", &path]);
        let _ = tx.send((
            ran,
            captured.stdout(),
            captured.stderr_text(),
            transport.requests().len(),
        ));
    });
    let (ran, out, stderr, requests) = rx.recv_timeout(common::HANG).expect("the replay ended");
    assert_eq!(ran, 0, "{stderr}");
    assert_eq!(out, stdout);
    assert_eq!(requests, 0);
    drop(reader);
}
