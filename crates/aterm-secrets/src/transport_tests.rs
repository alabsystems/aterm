// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::{Read, Write};
use std::path::Path;

const HANG_DETECTOR: Duration = Duration::from_secs(60);

fn home() -> &'static Path {
    Path::new("/Users//someone")
}

fn key() -> [u8; 32] {
    let mut key = [0u8; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        // Bytes that make `+`, `/` and padding appear in the base64.
        *byte = (index as u8).wrapping_mul(37).wrapping_add(251);
    }
    key
}

fn recipient() -> String {
    test_recipient(0)
}

/// A reply states a records bound only in one header that parses: one sent twice, even with
/// the same value, states none, as does a value that is not a bound's or no header at all.
///
/// CONTROL (in the test): the same value sent once is the bound it states.
#[test]
fn a_reply_states_a_records_bound_only_in_one_header_that_parses() {
    let bound = crate::crossing::RecordsBound::of(4_096, true);
    let value = bound.value();
    assert_eq!(
        ReplyBound::of([value.as_str()].into_iter()),
        ReplyBound::Stated(bound),
        "control: one header"
    );
    assert_eq!(
        ReplyBound::of([value.as_str(), value.as_str()].into_iter()),
        ReplyBound::Unstated
    );
    let other = crate::crossing::RecordsBound::of(4_096, false).value();
    assert_eq!(
        ReplyBound::of([value.as_str(), other.as_str()].into_iter()),
        ReplyBound::Unstated
    );
    assert_eq!(
        ReplyBound::of(["1 4096 open"].into_iter()),
        ReplyBound::Unstated
    );
    assert_eq!(ReplyBound::of(std::iter::empty()), ReplyBound::Unstated);
}

#[test]
fn a_url_parses_with_and_without_a_pin_in_every_base64_spelling() {
    let (source, pin) =
        parse_source("  http://alab-secrets.local:7484/v1/alab \n", home()).expect("url");
    assert_eq!(
        source,
        Source::Url("http://alab-secrets.local:7484/v1/alab".to_owned())
    );
    assert_eq!(pin, None);
    let standard = crate::b64::encode_padded(&key());
    assert!(
        standard.contains(['+', '/']) && standard.ends_with('='),
        "{standard}"
    );
    let urlsafe = standard.replace('+', "-").replace('/', "_");
    for spelling in [
        standard.clone(),
        standard.trim_end_matches('=').to_owned(),
        urlsafe.clone(),
        urlsafe.trim_end_matches('=').to_owned(),
    ] {
        let (source, pin) =
            parse_source(&format!("https://vault.example/v1/alab#{spelling}"), home())
                .expect("pinned url");
        assert_eq!(
            source,
            Source::Url("https://vault.example/v1/alab".to_owned())
        );
        assert_eq!(pin, Some(key()), "{spelling}");
    }
    let extra = format!("{standard}A");
    for bad in ["", "AAAA", "not base64!", extra.as_str()] {
        assert!(
            parse_source(&format!("http://h/v1/alab#{bad}"), home()).is_err(),
            "{bad:?}"
        );
    }
    let mixed = format!("-{}", &standard[1..]);
    if mixed.contains(['+', '/']) {
        assert!(parse_source(&format!("http://h/v1/alab#{mixed}"), home()).is_err());
    }
}

#[test]
fn paths_parse_and_anything_else_is_refused_with_a_hint() {
    assert_eq!(
        parse_source("~/vaults/alab.vault", home())
            .expect("home path")
            .0,
        Source::Path(PathBuf::from("/Users//someone/vaults/alab.vault"))
    );
    assert_eq!(
        parse_source("/Volumes/x/alab.vault", home()).expect("absolute"),
        (Source::Path(PathBuf::from("/Volumes/x/alab.vault")), None)
    );
    for (text, hint) in [
        ("", "no source"),
        ("~", "~/"),
        ("~/", "home directory"),
        ("~other/x", "~/"),
        ("alab.vault", "absolute"),
        ("alab-secrets.local:7484/v1/alab", "http://"),
        ("ftp://host/v1/alab", "only http"),
        ("file:///tmp/alab.vault", "only http"),
        ("http://", "not usable"),
        ("http://user:hunter2@host/v1/alab", "not usable"),
    ] {
        let error = parse_source(text, home()).expect_err(text);
        assert!(error.0.contains(hint), "{text:?}: {error}");
        assert!(!error.0.contains("hunter2"), "{error}");
    }
}

#[test]
fn the_vault_name_is_the_last_segment_or_the_stem() {
    let url = |text: &str| Source::Url(text.to_owned());
    assert_eq!(
        vault_name(&url("http://h:7484/v1/alab")).as_deref(),
        Ok("alab")
    );
    assert_eq!(
        vault_name(&url("http://h/v1/my-vault?x=1")).as_deref(),
        Ok("my-vault")
    );
    assert!(vault_name(&url("http://h/v1/alab/")).is_err());
    assert!(vault_name(&url("http://h/v1/Alab")).is_err());
    assert!(vault_name(&url("http://h")).is_err());
    let path = |text: &str| Source::Path(PathBuf::from(text));
    assert_eq!(vault_name(&path("/x/alab.vault")).as_deref(), Ok("alab"));
    assert_eq!(vault_name(&path("/x/alab")).as_deref(), Ok("alab"));
    assert!(vault_name(&path("/x/My Secrets.txt")).is_err());
}

#[test]
fn query_and_join_urls_compose() {
    assert_eq!(
        with_query("http://h/v1/a", "after=3"),
        "http://h/v1/a?after=3"
    );
    assert_eq!(
        with_query("http://h/v1/a?k=v#f", "after=3"),
        "http://h/v1/a?k=v&after=3"
    );
    assert_eq!(join_url("http://h/v1/a"), "http://h/v1/a/join");
    assert_eq!(join_url("http://h/v1/a?k=v"), "http://h/v1/a/join?k=v");
    assert_eq!(
        describe("http://h:7484/v1/a?token=secret#pin"),
        "http://h:7484/v1/a"
    );
}

#[test]
fn source_debug_omits_url_credentials() {
    let query = "synthetic-vault-query-credential";
    let fragment = "synthetic-vault-fragment-credential";
    for url in [
        format!("https://vault.example/v1/alab?token={query}#{fragment}"),
        format!("https://{query}@vault.example/v1/alab#{fragment}"),
    ] {
        let source = Source::Url(url);
        for shown in [format!("{source:?}"), format!("{source:#?}")] {
            assert!(!shown.contains(query));
            assert!(!shown.contains(fragment));
        }
    }
    assert_eq!(
        format!(
            "{:?}",
            Source::Url("https://vault.example/v1/alab?token=x".to_owned())
        ),
        "Url(\"https://vault.example/v1/alab\")"
    );
    assert_eq!(
        format!("{:?}", Source::Path(PathBuf::from("/vault/alab"))),
        "Path(\"/vault/alab\")"
    );
}

#[test]
fn a_reason_is_one_short_printable_line() {
    assert_eq!(reason(b"seq 3 is not newer\nmore"), "seq 3 is not newer");
    assert_eq!(reason(b"bad \x1b[31mred\x07\r\n"), "bad ?[31mred?");
    assert_eq!(reason("caf\u{e9}".as_bytes()), "caf??");
    assert_eq!(reason(&[b'x'; 500]).len(), 200);
    assert_eq!(reason(b""), "");
}

/// This machine's own failure to reach a server keeps a remedy a server's reason would have
/// no room for (a Linux `.local` lookup's), within its own cap.
#[test]
fn a_failure_to_reach_the_server_keeps_its_remedy() {
    let hint = format!(
        "dns: alab-secrets.local: Name or service not known ({})",
        "x".repeat(250)
    );
    let error = aterm_http::Error::Io(std::io::Error::other(hint.clone()));
    let said = transport_error("fetching", "http://alab-secrets.local:7484/v1/alab", &error).0;
    assert!(said.ends_with(&format!("{})", "x".repeat(250))), "{said}");
    assert_eq!(one_line(&[b'x'; 900], ERROR_CHARS).len(), ERROR_CHARS);
    // What the server sent keeps a server's cap.
    let sent = aterm_http::Error::Protocol("y".repeat(900));
    let said = transport_error("fetching", "http://alab-secrets.local:7484/v1/alab", &sent).0;
    assert!(said.len() < 300, "{said}");
}

#[cfg(unix)]
#[test]
fn a_path_is_fetched_and_published_atomically_and_world_readably() {
    use std::os::unix::fs::PermissionsExt;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let path = root.path().join("alab.vault");
    let source = Source::Path(path.clone());
    assert_eq!(
        fetch(&source, None, HANG_DETECTOR).expect("missing"),
        Fetched::Missing
    );
    publish(&source, b"alab-vault/1\n").expect("published");
    let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
    assert_eq!(mode & 0o777, 0o644);
    assert_eq!(
        fetch(&source, Some(9), HANG_DETECTOR).expect("fetched"),
        Fetched::Body(b"alab-vault/1\n".to_vec())
    );
    let link = root.path().join("link.vault");
    std::os::unix::fs::symlink(&path, &link).expect("link");
    assert!(fetch(&Source::Path(link), None, HANG_DETECTOR).is_err());
}

/// Version `seq` of vault `alab`, one admin locker holding `body`, signed.
#[cfg(unix)]
fn version(seq: u64, body: &[u8]) -> Vec<u8> {
    use crate::envelope::{Locker, Unsigned, sign, test_armor};
    let unsigned = Unsigned {
        name: "alab".to_owned(),
        seq,
        time: 1_792_530_000 + seq,
        handoff: Vec::new(),
        parents: Vec::new(),
        lockers: vec![Locker {
            label: "admin".to_owned(),
            armored: test_armor(body),
        }],
    };
    sign(&unsigned, &[7u8; 32]).expect("signed")
}

#[cfg(unix)]
#[test]
fn a_path_takes_only_a_newer_version_of_its_vault() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let path = root.path().join("alab.vault");
    let source = Source::Path(path.clone());
    let held = |bytes: &[u8]| {
        assert_eq!(
            fetch(&source, None, HANG_DETECTOR).expect("fetched"),
            Fetched::Body(bytes.to_vec())
        );
    };
    let first = version(41, b"one admin's text");
    publish(&source, &first).expect("published");
    let error = publish(&source, &version(41, b"another admin's text")).expect_err("not newer");
    assert!(error.0.contains("not newer than seq 41"), "{error}");
    assert!(publish(&source, &version(40, b"older")).is_err());
    held(&first);
    publish(&source, &first).expect("the same bytes again change nothing");
    let next = version(42, b"on top");
    publish(&source, &next).expect("newer");
    held(&next);
    std::fs::write(&path, b"not an envelope\n").expect("write");
    publish(&source, &first).expect("replaced");
    held(&first);
}

/// Version `seq` of vault `alab` signed by `seed`, carrying `handoff`, one admin locker
/// holding `body`.
#[cfg(unix)]
fn version_by(
    seq: u64,
    seed: &[u8; 32],
    handoff: Vec<([u8; 32], [u8; 64])>,
    body: &[u8],
) -> Vec<u8> {
    use crate::envelope::{Locker, Unsigned, sign, test_armor};
    let unsigned = Unsigned {
        name: "alab".to_owned(),
        seq,
        time: 1_792_530_000 + seq,
        handoff,
        parents: Vec::new(),
        lockers: vec![Locker {
            label: "admin".to_owned(),
            armored: test_armor(body),
        }],
    };
    sign(&unsigned, seed).expect("signed")
}

/// A path judges a version as a server does (P3's `Takeover::of`, server.rs `takes_over`): one
/// whose signer the held version's key never handed off to is refused as a server's 403 is
/// ([`ErrorKind::Refused`]: nothing was stored), and so is one that drops a link of the held
/// version's handoff chain; neither is written, and a handoff the held key endorsed takes
/// over.
///
/// NEGATIVE CONTROL, run in the test: [`PATH_SEQUENCE_ONLY`] (`path-sequence-only`, the
/// path's rule before it asked the kernels: sequence and parents alone) writes both.
#[cfg(unix)]
#[test]
fn a_path_refuses_what_a_server_refuses() {
    use crate::envelope::{endorse, signer_public};
    let (seven, nine) = ([7u8; 32], [9u8; 32]);
    let first = version(41, b"one admin's text");
    let stranger = version_by(42, &[8u8; 32], Vec::new(), b"a stranger's text");
    let to_nine = signer_public(&nine).expect("public");
    let link = (
        signer_public(&seven).expect("public"),
        endorse(&seven, "alab", &to_nine).expect("endorsed"),
    );
    let rotated = version_by(43, &nine, vec![link], b"rotated");
    let dropped = version_by(44, &nine, Vec::new(), b"the chain dropped");
    // A fresh path holding `first`: its file, and what each publish answered.
    let run = |cuts: &[&Vec<u8>]| {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let path = root.path().join("alab.vault");
        let source = Source::Path(path.clone());
        publish(&source, &first).expect("published");
        let answers: Vec<_> = cuts.iter().map(|cut| publish(&source, cut)).collect();
        (answers, std::fs::read(&path).expect("vault"))
    };

    let (answers, held) = run(&[&stranger]);
    let error = answers[0]
        .as_ref()
        .expect_err("the pin refuses another key");
    assert_eq!(error.kind(), crate::ErrorKind::Refused, "{error}");
    assert_eq!(held, first);

    let (answers, held) = run(&[&rotated]);
    answers[0]
        .as_ref()
        .expect("a handoff the held key endorsed takes over");
    assert_eq!(held, rotated);

    let (answers, held) = run(&[&rotated, &dropped]);
    let error = answers[1].as_ref().expect_err("the chain only grows");
    assert_eq!(error.kind(), crate::ErrorKind::Refused, "{error}");
    assert!(error.0.contains("handoff chain only grows"), "{error}");
    assert_eq!(held, rotated);

    PATH_SEQUENCE_ONLY.with(|cell| cell.set(true));
    let (stranger_taken, dropped_taken) = (run(&[&stranger]), run(&[&rotated, &dropped]));
    PATH_SEQUENCE_ONLY.with(|cell| cell.set(false));
    assert!(
        stranger_taken.0[0].is_ok() && stranger_taken.1 == stranger,
        "CONTROL: by sequence and parents alone, a stranger's key takes the path over"
    );
    assert!(
        dropped_taken.0[1].is_ok() && dropped_taken.1 == dropped,
        "CONTROL: by sequence and parents alone, a cut that drops the chain takes it over"
    );
}

/// A vault path whose folder is missing (mistyped, or a volume not mounted) is refused naming
/// that folder, and nothing is written. Negative control: with the folder there, it publishes.
#[cfg(unix)]
#[test]
fn a_publish_into_a_missing_folder_names_the_folder() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let folder = root.path().join("shared");
    let path = folder.join("team.vault");
    let source = Source::Path(path.clone());
    let error = publish(&source, &version(1, b"first")).expect_err("no folder");
    assert!(
        error
            .0
            .starts_with(&format!("{}: no such folder", folder.display()))
            && !error.0.contains(".lock"),
        "{error}"
    );
    assert!(!folder.exists());
    std::fs::create_dir(&folder).expect("folder");
    publish(&source, &version(1, b"first")).expect("published");
    assert!(path.exists());
}

/// While another publisher holds `<path>.lock`, a publish or replace to the path is refused,
/// naming the lock and its holder and leaving both as they were; a publish leaves no lock of
/// its own. Negative control: a publish that took no lock would land the held-lock seq 42.
#[cfg(unix)]
#[test]
fn a_publish_to_a_path_another_publisher_holds_is_refused() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let path = root.path().join("alab.vault");
    let lock = root.path().join("alab.vault.lock");
    let source = Source::Path(path.clone());
    let first = version(41, b"one admin's text");
    publish(&source, &first).expect("published");
    assert!(!lock.exists(), "a publish leaves no lock behind");
    std::fs::write(&lock, b"m22.local 4242\n\x1b[2J").expect("another publisher");
    let error = publish(&source, &version(42, b"a revocation")).expect_err("held");
    assert!(
        error.0.contains("alab.vault.lock") && error.0.contains("(m22.local 4242)"),
        "{error}"
    );
    assert!(
        replace(&source, &version(42, b"a put-back"), &first).is_err(),
        "a replace takes the same lock"
    );
    assert_eq!(std::fs::read(&path).expect("vault"), first);
    assert_eq!(
        std::fs::read(&lock).expect("the holder's lock"),
        b"m22.local 4242\n\x1b[2J"
    );
    std::fs::remove_file(&lock).expect("released");
    if let Some(host) = aterm_uds::process::host_name() {
        for bad_pid in ["-1", "+99999999", "0"] {
            std::fs::write(&lock, format!("{host} {bad_pid}\n")).expect("malformed pid lock");
            assert!(
                !remove_dead(&lock),
                "signed or zero pid {bad_pid:?} must not be cleared as a gone process"
            );
            std::fs::remove_file(&lock).expect("cleanup");
        }
    }
    assert!(publish(&source, &version(40, b"older")).is_err());
    assert!(!lock.exists(), "a refused publish leaves no lock behind");
    publish(&source, &version(42, b"on top")).expect("newer");
    assert!(!lock.exists());
}

/// A one-shot HTTP stub: answers `response` to the first request and hands back the
/// request head it read.
fn stub(response: Vec<u8>) -> (String, std::thread::JoinHandle<String>) {
    let (url, server) = stub_each(vec![response]);
    (
        url,
        std::thread::spawn(move || server.join().expect("stub").remove(0)),
    )
}

/// An HTTP stub that answers each of `responses` to one request in turn and hands back
/// the request heads it read.
fn stub_each(responses: Vec<Vec<u8>>) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/v1/alab", listener.local_addr().expect("addr"));
    let server = std::thread::spawn(move || {
        let mut heads = Vec::new();
        for response in responses {
            let (mut stream, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .expect("accept");
            stream
                .set_read_timeout(Some(HANG_DETECTOR))
                .expect("timeout");
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).expect("read") == 0 {
                    break;
                }
                head.push(byte[0]);
            }
            let head = String::from_utf8_lossy(&head).into_owned();
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            let mut body = vec![0u8; length];
            stream.read_exact(&mut body).expect("body");
            stream.write_all(&response).expect("write");
            heads.push(head);
        }
        heads
    });
    (url, server)
}

#[test]
fn a_malformed_heard_answer_is_not_an_empty_history() {
    let tag = "ab".repeat(32);
    let good = format!("{tag} 123\n");
    let as_of = 100 + HEARD_COVERAGE_SECS;
    for body in [
        Vec::new(), // A prototype 200-empty route has no coverage contract.
        format!("aterm-heard-v1 100 {as_of} 0\n").into_bytes(), // Prototype had no anonymous coverage.
        format!("aterm-heard-v2 100 {as_of} 1 -\nbad-row\n").into_bytes(),
        format!("aterm-heard-v2 100 {as_of} 2 -\n{good}{tag} not-a-time\n").into_bytes(),
        format!("aterm-heard-v2 100 {as_of} 2 -\n{good}bad-tag 124\n").into_bytes(),
        format!("aterm-heard-v2 100 {as_of} 2 -\n{good}").into_bytes(), // Truncated at a whole row.
        format!("aterm-heard-v2 101 {as_of} 0 -\n").into_bytes(),       // One second short.
        format!("aterm-heard-v2 100 {as_of} 0 not-a-time\n").into_bytes(),
        vec![0xff],
    ] {
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(&body);
        let (url, server) = stub(response);
        assert!(
            fetch_heard(&url).is_err(),
            "malformed history became complete"
        );
        assert!(
            server
                .join()
                .expect("stub")
                .starts_with("GET /v1/alab/heard ")
        );
    }

    let body = format!(
        "aterm-heard-v2 100 {as_of} 3 126\n{good}{} 125\n{tag} 124\n",
        tag.to_ascii_uppercase()
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let (url, server) = stub(response.into_bytes());
    assert_eq!(
        fetch_heard(&url).expect("valid history"),
        Some(Heard {
            as_of,
            subscribers: std::collections::BTreeMap::from([(tag, 125)]),
            anonymous: Some(126),
        })
    );
    server.join().expect("stub");
}

#[test]
fn only_an_explicit_non_consuming_observer_sends_observer_purpose() {
    let response = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
    let (url, server) = stub_each(vec![response.clone(), response.clone(), response]);
    fetch_within(&url, None, HANG_DETECTOR).unwrap();
    fetch_after_nowait(&url, None, HANG_DETECTOR).unwrap();
    observe_after_nowait(&url, None, HANG_DETECTOR).unwrap();
    let heads = server.join().unwrap();
    for head in &heads[..2] {
        let head = head.to_ascii_lowercase();
        assert!(!head.contains("x-aterm-observer:"));
        assert!(!head.contains("x-aterm-subscriber:"));
    }
    assert!(heads[1].to_ascii_lowercase().contains("x-aterm-no-wait: 1"));
    assert!(
        heads[2]
            .to_ascii_lowercase()
            .contains("x-aterm-observer: 1")
    );
}

/// [`observe_after_nowait_within`] reads at most its limit of any answer, as a server's
/// non-consuming pull: one that declares a longer length, or a chunked one that runs
/// longer, is [`Pulled::Larger`], whatever its status; one within the limit is read whole.
#[test]
fn a_bounded_pull_reads_at_most_its_limit_of_any_answer() {
    let answer = |status: &str, body: &[u8]| {
        let mut answer = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        answer.extend_from_slice(body);
        answer
    };
    let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n\
        10\r\n0123456789abcdef\r\n10\r\n0123456789abcdef\r\n0\r\n\r\n"
        .to_vec();
    let (url, server) = stub_each(vec![
        answer("200 OK", &[7; 64]),
        chunked,
        answer("404 Not Found", &[b'x'; 64]),
        answer("200 OK", &[7; 16]),
    ]);
    for why in [
        "a declared length over the limit",
        "a chunked body over the limit",
        "a 404 page over the limit",
    ] {
        assert_eq!(
            observe_after_nowait_within(&url, Some(3), HANG_DETECTOR, 16).expect(why),
            Pulled::Larger,
            "{why}"
        );
    }
    assert_eq!(
        observe_after_nowait_within(&url, Some(3), HANG_DETECTOR, 16).expect("within"),
        Pulled::Fetched(Fetched::Body(vec![7; 16])),
        "a body within the limit is read whole"
    );
    for head in server.join().expect("stub") {
        assert!(head.starts_with("GET /v1/alab?after=3 "), "{head}");
        let head = head.to_ascii_lowercase();
        assert!(head.contains("x-aterm-no-wait: 1"), "{head}");
        assert!(head.contains("x-aterm-observer: 1"), "{head}");
    }
}

fn too_many(retry_after: Option<u64>) -> Vec<u8> {
    let header = retry_after.map_or_else(String::new, |secs| format!("Retry-After: {secs}\r\n"));
    format!("HTTP/1.1 429 Too Many Requests\r\n{header}Content-Length: 9\r\n\r\nslow down")
        .into_bytes()
}

#[test]
fn a_join_queue_read_obeys_its_shared_deadline() {
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/alab", silent.local_addr().unwrap());
    let started = std::time::Instant::now();
    let error = wait_joins_from_until(&url, None, started + Duration::from_millis(200))
        .expect_err("the silent queue cannot hold the reader past its deadline");
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert!(started.elapsed() < HANG_DETECTOR);
}

#[test]
fn bounded_join_stops_before_a_retry_that_outlasts_its_deadline() {
    let line = format!("{} m22 abcd-ef23-ghij-k234", recipient());
    let (url, server) = stub_each(vec![too_many(Some(10))]);
    let pauses = std::cell::RefCell::new(Vec::new());
    let error = post_join_bounded(
        &url,
        &line,
        &|wait| pauses.borrow_mut().push(wait),
        Some(std::time::Instant::now() + Duration::from_secs(5)),
    )
    .expect_err("retry cannot finish before deadline");
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert!(pauses.borrow().is_empty());
    assert_eq!(server.join().expect("stub").len(), 1);

    let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/v1/alab", silent.local_addr().unwrap());
    let started = std::time::Instant::now();
    let error = post_join_until(&url, &line, started + Duration::from_millis(200))
        .expect_err("silent peer ends at deadline");
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert!(started.elapsed() < HANG_DETECTOR);
}

#[test]
fn a_join_sits_out_the_change_gap_once_and_no_other_refusal() {
    let line = format!("{} m22 abcd-ef23-ghij-k234", recipient());
    let no_content = b"HTTP/1.1 204 No Content\r\n\r\n".to_vec();
    let paused = std::cell::RefCell::new(Vec::new());
    let pause = |wait: Duration| paused.borrow_mut().push(wait);
    // The gap's 429 names the seconds left: sat out, and the line sent again.
    let (url, server) = stub_each(vec![too_many(Some(7)), no_content.clone()]);
    post_join_pausing(&url, &line, &pause).expect("taken after the wait");
    let heads = server.join().expect("stub");
    assert_eq!(heads.len(), 2);
    assert!(
        heads
            .iter()
            .all(|head| head.starts_with("POST /v1/alab/join "))
    );
    assert_eq!(*paused.borrow(), [Duration::from_secs(7)]);
    // Only once: a second 429 is the answer.
    paused.borrow_mut().clear();
    let (url, server) = stub_each(vec![too_many(Some(1)), too_many(Some(1))]);
    let error = post_join_pausing(&url, &line, &pause).expect_err("refused twice");
    assert!(error.0.contains("429"), "{error}");
    assert_eq!(server.join().expect("stub").len(), 2);
    assert_eq!(*paused.borrow(), [Duration::from_secs(1)]);
    // CONTROL: the queue's caps name no wait, and a wait past the gap is no gap's: each
    // refused at once, with no pause.
    paused.borrow_mut().clear();
    for refusal in [too_many(None), too_many(Some(30))] {
        let (url, server) = stub_each(vec![refusal]);
        let error = post_join_pausing(&url, &line, &pause).expect_err("refused");
        assert!(error.0.contains("429: slow down"), "{error}");
        assert_eq!(server.join().expect("stub").len(), 1);
    }
    assert!(paused.borrow().is_empty());
}

#[test]
fn a_refusal_names_the_status_and_a_sanitized_reason() {
    let mut response = b"HTTP/1.1 409 Conflict\r\nContent-Length: 300\r\n\r\n".to_vec();
    let mut body = b"seq 2 is not newer than 5 \x1b[2J".to_vec();
    body.resize(300, b'z');
    response.extend_from_slice(&body);
    let (url, server) = stub(response);
    let error = publish(&Source::Url(url), b"alab-vault/1\n").expect_err("refused");
    let head = server.join().expect("stub");
    assert!(head.starts_with("PUT /v1/alab HTTP/1.1\r\n"), "{head}");
    assert!(error.0.contains("409"), "{error}");
    assert!(
        error.0.contains("seq 2 is not newer than 5 ?[2J"),
        "{error}"
    );
    assert!(!error.0.contains('\x1b'), "{error}");
    assert!(error.0.len() < 300, "{error}");
}

#[test]
fn a_malformed_answer_is_quoted_as_one_short_printable_line() {
    let mut response = b"HTTP/1.1 2x \x1b[2J".to_vec();
    response.resize(2000, b'z');
    response.extend_from_slice(b"\r\nsecond: line\r\n\r\n");
    let (url, server) = stub(response);
    let error = fetch(&Source::Url(url), None, HANG_DETECTOR).expect_err("malformed");
    server.join().expect("stub");
    assert!(error.0.starts_with("fetching http://127.0.0.1:"), "{error}");
    assert!(error.0.len() < 300, "{error}");
    assert!(
        error.0.bytes().all(|b| (0x20..0x7f).contains(&b)),
        "{error}"
    );
    assert!(!error.0.contains("second"), "{error}");
}

/// A long-poll's 204 says the seq the server holds by its `ETag`, which a machine holding a
/// newer version reads; a 204 without one, or with one that names no seq, says none.
#[test]
fn a_long_poll_asks_after_the_seq_and_reads_no_newer() {
    let (url, server) = stub(b"HTTP/1.1 204 No Content\r\nETag: \"40\"\r\n\r\n".to_vec());
    let fetched = fetch_as_observed(&Source::Url(url.clone()), Some(41), HANG_DETECTOR, None)
        .expect("fetched");
    assert_eq!(fetched.fetched, Fetched::NoNewer(Some(40)));
    let reply = fetched.http204.expect("actual completed204");
    assert!(reply.responded_at_ms() > 0);
    assert!(reply.matches_binding(request_binding(&url, Some(41), None)));
    assert!(!reply.matches_binding(request_binding(&url, Some(40), None)));
    assert!(!reply.matches_binding(request_binding(&url, Some(41), Some("other-subscriber"))));
    assert!(!reply.matches_binding(request_binding(
        "http://127.0.0.1:1/v1/other",
        Some(41),
        None
    )));
    let head = server.join().expect("stub");
    assert!(
        head.starts_with("GET /v1/alab?after=41 HTTP/1.1\r\n"),
        "{head}"
    );
    assert!(
        !head.to_ascii_lowercase().contains("content-length"),
        "{head}"
    );
    for etag in ["", "ETag: 40\r\n", "ETag: \"\"\r\n", "ETag: \"+40\"\r\n"] {
        let (url, server) = stub(format!("HTTP/1.1 204 No Content\r\n{etag}\r\n").into_bytes());
        let fetched =
            fetch_as_observed(&Source::Url(url), Some(41), HANG_DETECTOR, None).expect("fetched");
        assert_eq!(fetched.fetched, Fetched::NoNewer(None), "{etag:?}");
        assert!(
            fetched.http204.is_some(),
            "no validETag is needed to observe a response"
        );
        server.join().expect("stub");
    }
}

/// Over real sockets: a port nobody listens on and a server's `503` say what failed, and a
/// server that takes the request and never answers fails once the request's deadline passes
/// (here a short one, not [`get_bounded`]'s `wait` plus [`FETCH_MARGIN`]).
#[test]
fn a_get_says_what_failed() {
    let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/v1/alab", closed.local_addr().expect("addr"));
    drop(closed);
    let refused =
        get_bounded(&url, None, HANG_DETECTOR, Some(LOOK_CONNECT)).expect_err("no listener");
    assert!(
        refused.why.0.starts_with(&format!("fetching {url}")),
        "{refused:?}"
    );

    let (url, server) =
        stub(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n".to_vec());
    let busy = get_bounded(&url, None, HANG_DETECTOR, Some(LOOK_CONNECT)).expect_err("503");
    server.join().expect("stub");
    assert!(busy.why.0.contains("503"), "{busy:?}");

    let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/v1/alab", silent.local_addr().expect("addr"));
    let unanswered = get_via(
        &client(Duration::from_millis(250)),
        &url,
        None,
        Some(LOOK_CONNECT),
        None,
    )
    .expect_err("silent");
    assert_eq!(unanswered.why.kind(), ErrorKind::TimedOut, "{unanswered:?}");
    drop(silent);
}

/// A connector that answers every connect as a lookup with no answer, and keeps the time
/// its deadline left it.
#[derive(Debug, Default)]
struct UnansweredLookup {
    left: std::sync::Mutex<Vec<Duration>>,
}

impl aterm_http::Connect for UnansweredLookup {
    fn connect(
        &self,
        _host: &str,
        _port: u16,
        deadline: aterm_http::Deadline,
    ) -> std::io::Result<std::net::TcpStream> {
        let left = deadline.remaining().unwrap_or_default();
        self.left.lock().expect("lock").push(left);
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("dns: no answer within {}ms", left.as_millis()),
        ))
    }
}

/// A look's try gives its DNS lookup and connect [`LOOK_CONNECT`] of a much longer
/// request deadline. Negative control: with no connect bound the lookup has the whole
/// deadline.
#[test]
fn a_looks_lookup_and_connect_get_the_connect_bound() {
    let held = std::sync::Arc::new(UnansweredLookup::default());
    let client = client(HANG_DETECTOR.saturating_add(FETCH_MARGIN)).with_connector(held.clone());
    let url = "http://alab-secrets.local:7484/v1/alab";
    let bounded = get_via(&client, url, None, Some(LOOK_CONNECT), None).expect_err("held lookup");
    assert!(
        bounded
            .why
            .0
            .starts_with("fetching http://alab-secrets.local:7484/v1/alab: dns: no answer within"),
        "{bounded:?}"
    );
    let unbounded = get_via(&client, url, None, None, None).expect_err("held lookup");
    assert_eq!(unbounded.why.kind(), ErrorKind::TimedOut, "{unbounded:?}");
    let left = held.left.lock().expect("lock").clone();
    assert_eq!(left.len(), 2);
    assert!(left[0] <= LOOK_CONNECT, "{left:?}");
    assert!(
        left[1] > LOOK_CONNECT,
        "the request's own deadline: {left:?}"
    );
}

/// A look ([`get_within`]) gives up on a server that takes its connect and never answers
/// at its own total, with no [`FETCH_MARGIN`] after it. Negative control: a look on a
/// fetch's deadline (the total plus the margin, under test longer than the hang detector)
/// is still waiting at the detector.
#[test]
fn a_look_gives_up_on_a_silent_server_at_its_total_with_no_margin() {
    let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/v1/alab", silent.local_addr().expect("addr"));
    let total = Duration::from_millis(200);
    let started = std::time::Instant::now();
    let error = get_within(&url, total).expect_err("a server that never answers");
    let took = started.elapsed();
    println!(
        "MEASURE a look at a silent server gave up after {} ms (total {} ms)",
        took.as_millis(),
        total.as_millis()
    );
    assert!(error.0.contains("deadline"), "{error:?}");
    assert!(
        took < HANG_DETECTOR,
        "the look waited the fetch margin past its total: {took:?}"
    );
    drop(silent);
}

/// A bounded wait's try ([`fetch_within`]) ends at its own total on a server that holds it,
/// with no [`FETCH_MARGIN`] after it: a long-poll still held then heard nothing newer (an
/// older server holds one longer than this asker waits). Negative controls: a try on a
/// fetch's deadline (the total plus the margin, under test longer than the hang detector)
/// is still waiting at the detector; a GET that is no long-poll (no `after`), which a
/// server answers at once, failing so is a timeout.
#[test]
fn a_bounded_fetch_ends_at_its_total_as_no_news() {
    let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/v1/alab", silent.local_addr().expect("addr"));
    let total = Duration::from_millis(200);
    let started = std::time::Instant::now();
    let held =
        fetch_within_as_observed(&url, Some(3), total, None).expect("a held long-poll is no news");
    let took = started.elapsed();
    assert_eq!(held.fetched, Fetched::NoNewer(None));
    assert!(
        held.http204.is_none(),
        "a sent request timeout is no response"
    );
    assert!(
        took < HANG_DETECTOR,
        "the try waited the fetch margin past its total: {took:?}"
    );
    let error = fetch_within(&url, None, total).expect_err("a look that is never answered");
    assert_eq!(error.kind(), ErrorKind::TimedOut, "{error:?}");
    drop(silent);
}

/// `text` with its last character changed to another bech32 one: a broken checksum.
fn flip_last(text: &str) -> String {
    let mut out = text.to_owned();
    let last = out.pop();
    out.push(if last == Some('q') { 'p' } else { 'q' });
    out
}

#[test]
fn join_lines_are_validated() {
    let line = format!("{} m22 abcd-ef23-ghij-k234", recipient());
    let request = parse_join_line(&line).expect("valid");
    assert_eq!(request.host, "m22");
    assert_eq!(request.code, "abcd-ef23-ghij-k234");
    assert_eq!(request.time, None);
    assert_eq!(request.proof, None);
    assert_eq!(request.line(), line);
    let listed = parse_join_line(&format!("{line} 1792530000")).expect("listed");
    assert_eq!(listed.time, Some(1_792_530_000));
    assert_eq!(listed.listed(), format!("{line} 1792530000"));
    let proof = crate::b64::encode_padded(&[7u8; 32]);
    let proved = parse_join_line(&format!("{line} proof={proof}")).expect("proved");
    assert_eq!(proved.proof.as_deref(), Some(proof.as_str()));
    assert_eq!(proved.line(), format!("{line} proof={proof}"));
    let listed = parse_join_line(&format!("{line} 1792530000 proof={proof}")).expect("listed");
    assert_eq!(listed.time, Some(1_792_530_000));
    assert_eq!(listed.proof.as_deref(), Some(proof.as_str()));
    // An older server's LAN markers (` lan=1` before 0.148, ` home=<tag>` from 0.148 and
    // 0.149) parse and are read past: the request stays listed, and nothing of the marker
    // survives into what this Mac lists or posts.
    let tag = "0123456789abcdef";
    for (old, without) in [
        (
            format!("{line} 1792530000 lan=1 proof={proof}"),
            format!("{line} 1792530000 proof={proof}"),
        ),
        (
            format!("{line} 1792530000 home={tag}"),
            format!("{line} 1792530000"),
        ),
        (
            format!("{line} 1792530000 lan=1 home={tag} proof={proof}"),
            format!("{line} 1792530000 proof={proof}"),
        ),
    ] {
        let marked = parse_join_line(&old).expect("an older server's listing");
        assert_eq!(marked.listed(), without, "{old}");
        assert_eq!(marked.line(), parse_join_line(&without).unwrap().line());
        assert_eq!(marked.host, "m22", "{old}");
    }
    for bad in [
        format!("{line} lan=1"),
        format!("{line} home={tag}"),
        format!("{line} lan=1 home={tag}"),
        format!("{line} 1792530000 home=0123456789ABCDEF"),
        format!("{line} 1792530000 home=0123456789abcde"),
        format!("{line} 1792530000 home="),
        format!("{line} 1792530000 home={tag} home={tag}"),
        format!("{line} 1792530000 lan=0"),
        format!("{line} 1792530000 lan=1 lan=1"),
        format!("{line} 1792530000 lan=true"),
        format!("{line} proof="),
        format!("{line} proof={}", crate::b64::encode_padded(&[7u8; 31])),
        format!("{line} proof={proof} 1792530000"),
        format!("{line} proof={proof}x"),
        String::new(),
        line.replace(' ', "  "),
        format!("{line} x"),
        format!("{line} 1 2"),
        line.replace("m22", "M22"),
        line.replace("m22", "everyone"),
        line.replace("m22", "../x"),
        line.replace("abcd-ef23-ghij-k234", "abcd-ef21-ghij-k234"),
        line.replace("abcd-ef23-ghij-k234", "abcd-ef23"),
        line.replace("abcd-ef23-ghij-k234", "abcdef23ghijk234"),
        line.replace("abcd-ef23-ghij-k234", "abcd-ef23-ghij-k23-"),
        line.replace("age1", "age2"),
        format!("{}q m22 abcd-ef23-ghij-k234", recipient()),
        format!("{} m22 abcd-ef23-ghij-k234", flip_last(&recipient())),
    ] {
        assert!(parse_join_line(&bad).is_err(), "{bad:?}");
    }
    assert!(post_join("http://127.0.0.1:9/v1/alab", "nonsense").is_err());
}

/// A stand-in LAN broker on a fresh socket that answers one resolve of a local name with
/// `addrs`, and hands back the name it was asked; the seam that points this thread there.
#[cfg(target_os = "macos")]
fn resolving_broker(
    addrs: Vec<IpAddr>,
) -> (
    aterm_uds::lan::Seam,
    aterm_tempfile::TempDir,
    std::thread::JoinHandle<Option<String>>,
) {
    use aterm_uds::lan::wire::{self, Frame, Reader, Request};
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let path = root.path().join("broker.sock");
    let listener = aterm_uds::CtlListener::bind(&path).expect("bind");
    let served = std::thread::spawn(move || {
        let (stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .expect("accept");
        wire::send(
            &stream,
            &Frame::Hello {
                protocol: aterm_uds::lan::PROTOCOL,
                build: "stand-in".into(),
            },
        )
        .expect("hello");
        let mut reader = Reader::new(0);
        let deadline = Some(std::time::Instant::now() + HANG_DETECTOR);
        let hello = reader.frame(&stream, deadline).expect("their hello");
        assert!(matches!(hello, Some(Frame::Hello { .. })), "{hello:?}");
        match reader.frame(&stream, deadline).expect("request") {
            Some(Frame::Request(Request::Resolve { name, .. })) => {
                wire::send(&stream, &Frame::Addresses(addrs)).expect("answer");
                Some(name)
            }
            _ => None,
        }
    });
    let seam = aterm_uds::lan::Seam {
        socket: Some(path),
        ..aterm_uds::lan::Seam::default()
    };
    (seam, root, served)
}

/// A vault URL on a local name is resolved by the LAN broker, never by a lookup in this
/// process (which macOS's Local Network privacy would gate), and its server is then reached.
/// Negative control: with aterm-http's own connector in [`client`], the broker hears no
/// resolve (`vault-holder.local` then fails in this process's own lookup).
#[cfg(target_os = "macos")]
#[test]
fn a_local_name_is_resolved_by_the_lan_broker() {
    let (url, server) = stub(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec());
    let port = url
        .trim_start_matches("http://127.0.0.1:")
        .split('/')
        .next()
        .expect("port")
        .to_owned();
    let local = format!("http://vault-holder.local:{port}/v1/alab");
    let (seam, _root, broker) = resolving_broker(vec![IpAddr::from([127, 0, 0, 1])]);
    aterm_uds::lan::seam_here(seam, || get_within(&local, HANG_DETECTOR))
        .expect("the server the broker's answer named");
    assert_eq!(
        broker.join().expect("broker").as_deref(),
        Some("vault-holder.local")
    );
    let head = server.join().expect("stub");
    assert!(
        head.contains(&format!("vault-holder.local:{port}")),
        "{head}"
    );
}

/// With no LAN broker, a vault URL on a local name fails with the broker's own words, and
/// no lookup or connect happens here. Negative control: the same fetch by address reaches
/// the server.
#[cfg(target_os = "macos")]
#[test]
fn without_a_broker_a_local_name_fails_with_the_brokers_words() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let seam = aterm_uds::lan::Seam {
        socket: Some(root.path().join("absent.sock")),
        ..aterm_uds::lan::Seam::default()
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let failed = aterm_uds::lan::seam_here(seam, || {
        get_within(
            &format!("http://vault-holder.local:{port}/v1/alab"),
            HANG_DETECTOR,
        )
    })
    .expect_err("no broker");
    let words = aterm_uds::lan::Unavailable::NotInstalled.to_string();
    assert!(failed.0.contains(&words), "{failed:?}");
    assert!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        )
        .is_err(),
        "nothing connected"
    );
    drop(listener);
    let (url, server) = stub(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec());
    get_within(&url, HANG_DETECTOR).expect("by address");
    server.join().expect("stub");
}
