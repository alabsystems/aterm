// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The effect seam (the plan's §4.6.27): its live and simulated modes, the canonical forms,
//! the tape subset and its sink, and recording, replaying and rerecording over a scripted
//! transport, with each divergence typed.

use std::io::Read;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use agh::fx::canon::{self, BODY_TAPE_MAX, KEPT_HEADERS, Reply, keep, kept};
use agh::fx::tape::{self, TapeEndpoints, TapeHeader, TapeMode, TapeSink};
use agh::fx::{DOOR_EXIT, DOOR_GH, DOOR_SLEEP, DOOR_STDOUT, Fx, Mode};
use agh::identity::Credential;
use agh::policy::Tier;
use agh::wire::door::AuthHeader;
use agh::wire::scripted::{Scripted, ScriptedTransport};
use agh::wire::{
    Deadlines, Endpoints, Method, Phase, WireBody, WireError, WireKind, WireRequest, WireResponse,
};
use astream_effects::EffectRecord;

use agh::{Kind, Replay, Sub};

use crate::common::{HANG, scratch, tape_path};

/// 2026-01-01T00:00:00Z.
const NEW_YEAR_2026: u64 = 1_767_225_600;

fn get(limit: u64) -> WireRequest<'static> {
    WireRequest::new(
        Method::Get,
        "https://api.github.com/user",
        Vec::new(),
        limit,
        Deadlines::JSON,
    )
}

/// The kept names in the order `twenty_headers` sends them: reversed, so not sorted.
fn sent_order() -> Vec<&'static str> {
    KEPT_HEADERS.iter().rev().copied().collect()
}

/// Twenty headers: the fifteen kept ones in upper case and in reverse order, among five that
/// are not.
fn twenty_headers() -> Vec<(String, String)> {
    let mut headers = Vec::new();
    for (i, name) in KEPT_HEADERS.iter().enumerate().rev() {
        headers.push((name.to_ascii_uppercase(), format!("v{i}")));
        if i % 3 == 0 {
            headers.push((format!("X-Dropped-{i}"), "d".to_string()));
        }
    }
    assert_eq!(headers.len(), 20);
    headers
}

#[test]
fn simulated_time_moves_only_by_sleeps() {
    let fx = Fx::simulated(1_760_000_000);
    assert_eq!(fx.mode(), Mode::Simulated);
    assert_eq!(fx.mono().expect("mono"), Duration::ZERO);
    assert_eq!(fx.wall_secs().expect("wall"), 1_760_000_000);
    fx.sleep(Duration::from_secs(2)).expect("sleep");
    fx.sleep(Duration::from_millis(500)).expect("sleep");
    assert_eq!(fx.mono().expect("mono"), Duration::from_millis(2500));
    assert_eq!(fx.wall_secs().expect("wall"), 1_760_000_002);
    assert_eq!(
        fx.slept(),
        [Duration::from_secs(2), Duration::from_millis(500)]
    );
}

#[test]
fn a_simulated_hour_takes_no_real_time() {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let fx = Fx::simulated(1_760_000_000);
        let slept = fx.sleep(Duration::from_secs(3600)).is_ok();
        let _ = tx.send((slept, fx.mono().ok()));
    });
    let (slept, mono) = rx.recv_timeout(HANG).expect("the hour passed at once");
    assert!(slept);
    assert_eq!(mono, Some(Duration::from_secs(3600)));
}

#[test]
fn live_clocks() {
    let fx = Fx::live();
    assert_eq!(fx.mode(), Mode::Live);
    let a = fx.mono().expect("mono");
    let b = fx.mono().expect("mono");
    assert!(b >= a);
    assert!(fx.wall_secs().expect("wall") > NEW_YEAR_2026);
    assert!(fx.slept().is_empty());
}

#[test]
fn gh_keeps_the_kept_headers_and_reads_the_body() {
    let fx = Fx::simulated(1_760_000_000);
    let transport = ScriptedTransport::new();
    transport.push(Scripted::Answer {
        status: 200,
        headers: twenty_headers(),
        body: b"{\"login\":\"x\"}".to_vec(),
    });
    let mut answer = fx
        .gh_for_tests(&transport, &Endpoints::github_com(), get(1 << 20))
        .expect("no divergence")
        .expect("an answer");
    let names: Vec<&str> = answer.headers.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, sent_order());
    assert_eq!(answer.header("etag"), Some("v1"));
    let mut body = Vec::new();
    answer.body.read_to_end(&mut body).expect("the body");
    assert_eq!(body, b"{\"login\":\"x\"}");
}

// A body over the limit comes back cut one byte past it, with its head, for the pipeline's
// whole read to refuse with the status, request id and rate in hand.
#[test]
fn gh_cuts_a_body_over_the_limit_and_keeps_its_head() {
    let fx = Fx::simulated(1_760_000_000);
    let transport = ScriptedTransport::new();
    transport.answer(200, &[("x-github-request-id", "FAKE:1")], b"0123456789");
    let mut answer = fx
        .gh_for_tests(&transport, &Endpoints::github_com(), get(4))
        .expect("no divergence")
        .expect("the head, with the body cut");
    assert_eq!(answer.status, 200);
    assert_eq!(answer.header("x-github-request-id"), Some("FAKE:1"));
    let mut body = Vec::new();
    answer.body.read_to_end(&mut body).expect("the body");
    assert_eq!(body, b"01234");
}

#[test]
fn gh_hands_back_a_wire_error() {
    let fx = Fx::live();
    let transport = ScriptedTransport::new();
    let scripted = WireError {
        phase: Phase::Sent,
        kind: WireKind::Reset,
        detail: "reset".into(),
    };
    transport.push(Scripted::Fail(scripted.clone()));
    let error = fx
        .gh_for_tests(&transport, &Endpoints::github_com(), get(1 << 20))
        .expect("no divergence")
        .expect_err("the scripted failure");
    assert_eq!(error, scripted);
}

#[test]
fn gh_stream_cuts_the_headers_and_leaves_the_body() {
    let fx = Fx::simulated(1_760_000_000);
    let transport = ScriptedTransport::new();
    transport.push(Scripted::Answer {
        status: 200,
        headers: twenty_headers(),
        body: b"streamed".to_vec(),
    });
    let mut answer = fx
        .gh_stream_for_tests(&transport, get(1))
        .expect("no divergence")
        .expect("an answer");
    let names: Vec<&str> = answer.headers.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, sent_order());
    assert_eq!(answer.header("etag"), Some("v1"));
    let mut body = Vec::new();
    answer.body.read_to_end(&mut body).expect("the body");
    assert_eq!(body, b"streamed");
}

#[test]
fn only_gh_stream_keeps_a_cdn_location_whole() {
    let signed = "https://objects.githubusercontent.com/a/b?X-Amz-Signature=s&e=1";
    let fx = Fx::simulated(1_760_000_000);
    let transport = ScriptedTransport::new();
    transport.answer(302, &[("Location", signed)], b"");
    transport.answer(302, &[("Location", signed)], b"");
    let streamed = fx
        .gh_stream_for_tests(&transport, get(1))
        .expect("no divergence")
        .expect("an answer");
    assert_eq!(streamed.header("location"), Some(signed));
    let whole = fx
        .gh_for_tests(&transport, &Endpoints::github_com(), get(1 << 20))
        .expect("no divergence")
        .expect("an answer");
    assert_eq!(
        whole.header("location"),
        Some("https://objects.githubusercontent.com/a/b?signed=redacted")
    );
}

#[test]
fn emit_and_finish_are_quiet_in_both_modes() {
    for fx in [Fx::live(), Fx::simulated(1_760_000_000)] {
        fx.emit_stdout(b"out\n").expect("emit");
        fx.finish(0).expect("finish");
    }
}

#[test]
fn kept_and_keep() {
    for name in KEPT_HEADERS {
        assert!(kept(name));
        assert!(kept(&name.to_ascii_uppercase()));
    }
    assert!(!kept("set-cookie"));
    let e = Endpoints::github_com();
    let headers = vec![
        (
            "Location".to_string(),
            "https://objects.githubusercontent.com/a/b?X-Amz-Signature=s&e=1".to_string(),
        ),
        ("Set-Cookie".to_string(), "c".to_string()),
        ("ETag".to_string(), "W/\"x\"".to_string()),
    ];
    assert_eq!(
        keep(&e, headers),
        [
            (
                "location".to_string(),
                "https://objects.githubusercontent.com/a/b?signed=redacted".to_string()
            ),
            ("etag".to_string(), "W/\"x\"".to_string()),
        ]
    );
    let api = vec![(
        "location".to_string(),
        "https://api.github.com/repositories/1?page=2".to_string(),
    )];
    assert_eq!(keep(&e, api.clone()), api);
}

// ---- The canonical request ----

/// A GET of `url` with `headers`.
fn get_with<'a>(url: &str, headers: &[(&str, &str)]) -> WireRequest<'a> {
    WireRequest::new(
        Method::Get,
        url,
        headers
            .iter()
            .map(|(n, v)| ((*n).to_string(), (*v).to_string()))
            .collect(),
        1 << 20,
        Deadlines::JSON,
    )
}

/// The canonical request as text.
fn canonical(req: &WireRequest<'_>, endpoints: &Endpoints) -> String {
    let bytes = canon::request(req, endpoints).expect("a canonical request");
    String::from_utf8(bytes).expect("UTF-8")
}

/// The error's kind, sub slug and exit.
fn shape(e: &agh::Error) -> (Kind, Option<String>, u8) {
    (e.kind, e.sub.slug(), e.exit())
}

/// A replay's stop: CouldNotRun, `replay.{slug}`, exit 3.
fn replay_stop(slug: &str) -> (Kind, Option<String>, u8) {
    (Kind::CouldNotRun, Some(format!("replay.{slug}")), 3)
}

#[test]
fn a_request_sorts_its_query_and_headers_and_drops_the_user_agent() {
    let req = get_with(
        "https://api.github.com/repos/o/r/releases?per_page=5&b=2&a=1&a=0",
        &[
            ("X-GitHub-Api-Version", "2022-11-28"),
            ("User-Agent", "agh/0 (test)"),
            ("Accept", "application/vnd.github+json"),
            ("Accept", "application/json"),
        ],
    );
    assert_eq!(
        canonical(&req, &Endpoints::github_com()),
        "GET /repos/o/r/releases?a=0&a=1&b=2&per_page=5\n\
         origin api\n\
         accept application/json\n\
         accept application/vnd.github+json\n\
         x-github-api-version 2022-11-28\n\
         as anonymous\n\
         body none"
    );
}

#[test]
fn a_request_names_each_origin_by_its_role() {
    let e = Endpoints::github_com();
    for (url, line) in [
        ("https://api.github.com/x", "GET /x\norigin api"),
        ("https://uploads.github.com/x", "GET /x\norigin uploads"),
        ("https://github.com/o/r", "GET /o/r\norigin web"),
        (
            "https://github-releases.githubusercontent.com/x",
            "GET /x\norigin cdn",
        ),
    ] {
        let text = canonical(&get_with(url, &[]), &e);
        assert_eq!(text, format!("{line}\nas anonymous\nbody none"));
    }
    let head = WireRequest::new(
        Method::Head,
        "https://api.github.com/x",
        Vec::new(),
        0,
        Deadlines::JSON,
    );
    assert!(canonical(&head, &e).starts_with("HEAD /x\n"));
}

#[test]
fn a_cdn_query_is_written_signed_redacted() {
    let req = get_with(
        "https://objects.githubusercontent.com/a/b?X-Amz-Signature=s&e=1",
        &[],
    );
    assert_eq!(
        canonical(&req, &Endpoints::github_com()),
        "GET /a/b?signed=redacted\norigin cdn\nas anonymous\nbody none"
    );
}

#[test]
fn a_reply_location_on_a_cdn_host_is_signed_redacted_and_one_on_the_api_is_kept() {
    let e = Endpoints::github_com();
    let cdn = keep(
        &e,
        vec![(
            "Location".to_string(),
            "https://release-assets.githubusercontent.com/github-production/1/2?sp=r&sig=abc&se=9"
                .to_string(),
        )],
    );
    assert_eq!(
        cdn,
        [(
            "location".to_string(),
            "https://release-assets.githubusercontent.com/github-production/1/2?signed=redacted"
                .to_string()
        )]
    );
    let api = vec![(
        "location".to_string(),
        "https://api.github.com/repositories/9/releases?page=2&per_page=100".to_string(),
    )];
    assert_eq!(keep(&e, api.clone()), api);
    let bytes = canon::reply(302, &cdn, b"");
    let text = String::from_utf8(bytes).expect("UTF-8");
    assert!(text.contains("/github-production/1/2?signed=redacted\n"));
    assert!(!text.contains("sig=abc"));
}

#[test]
fn a_credentialed_request_or_one_with_a_body_is_not_taped() {
    let e = Endpoints::github_com();
    let cred = Credential::for_tests("dev", Tier::Dev, agh_fake::token("canon").as_bytes());
    let header = AuthHeader::for_origin_for_tests(&cred, &e, e.api()).expect("a header");
    let mut credentialed = get_with("https://api.github.com/user", &[]);
    credentialed.auth = Some(&header);
    let refused = canon::request(&credentialed, &e).expect_err("credentialed");
    assert_eq!(shape(&refused), replay_stop("stream_not_taped"));
    assert!(refused.next.contains("--record"), "{}", refused.next);
    let mut with_body = get_with("https://api.github.com/user", &[]);
    with_body.method = Method::Post;
    with_body.body = WireBody::Bytes(b"{}");
    let refused = canon::request(&with_body, &e).expect_err("a body");
    assert_eq!(shape(&refused), replay_stop("stream_not_taped"));
}

#[test]
fn a_url_on_no_origin_is_a_protocol_error() {
    let e = Endpoints::github_com();
    for url in [
        "https://example.com/x",
        "http://api.github.com/x",
        "ftp://api.github.com/x",
    ] {
        let error = canon::request(&get_with(url, &[]), &e).expect_err(url);
        assert_eq!(
            (error.kind, error.sub.clone()),
            (Kind::CouldNotRun, Sub::Protocol)
        );
    }
}

// ---- The canonical reply and failure ----

/// The bodies every round trip test carries.
fn bodies() -> Vec<Vec<u8>> {
    vec![
        Vec::new(),
        b"\n".to_vec(),
        b"{\"a\":1}\n".to_vec(),
        b"no final newline".to_vec(),
        b"two\n\nlines\r\n\ttab\\back".to_vec(),
        vec![0x00, 0x1b, b'[', b'3', b'1', b'm', 0x07, 0x7f],
        vec![0xff, 0xfe, b'a', 0x80, 0xc3],
        "bidi \u{202e} and \u{2028} and \u{9b}".as_bytes().to_vec(),
    ]
}

/// The headers a reply test carries, already `keep`'s.
fn kept_headers() -> Vec<(String, String)> {
    vec![
        ("etag".to_string(), "W/\"x\"".to_string()),
        (
            "link".to_string(),
            "<https://api.github.com/x?page=2>; rel=\"next\"".to_string(),
        ),
        ("x-ratelimit-remaining".to_string(), String::new()),
    ]
}

/// Every wire kind.
const KINDS: [WireKind; 13] = [
    WireKind::Dns,
    WireKind::Connect,
    WireKind::Proxy,
    WireKind::Tls,
    WireKind::Trust,
    WireKind::Timeout,
    WireKind::Idle,
    WireKind::Reset,
    WireKind::Truncated,
    WireKind::TooLarge,
    WireKind::Protocol,
    WireKind::Revoked,
    WireKind::Local,
];

#[test]
fn a_reply_decodes_to_itself() {
    for body in bodies() {
        for headers in [Vec::new(), kept_headers()] {
            let bytes = canon::reply(200, &headers, &body);
            let back = canon::decode(&bytes)
                .expect("a reply agh wrote")
                .expect("an answer");
            assert_eq!(
                back,
                Reply {
                    status: 200,
                    headers: headers.clone(),
                    body: body.clone(),
                }
            );
        }
    }
    let empty = canon::reply(304, &[], b"");
    assert_eq!(empty, b"status 304\n\n");
}

#[test]
fn a_failure_decodes_to_itself() {
    for phase in [Phase::NotSent, Phase::Sent, Phase::Answered] {
        for kind in KINDS {
            for detail in ["", "connection reset by peer", "a  b"] {
                let error = WireError {
                    phase,
                    kind,
                    detail: detail.to_string(),
                };
                let back = canon::decode(&canon::failure(&error))
                    .expect("a failure agh wrote")
                    .expect_err("a failure");
                assert_eq!(back, error);
            }
        }
    }
}

#[test]
fn a_failure_detail_passes_the_shapes() {
    let token = agh_fake::token("detail");
    let error = WireError {
        phase: Phase::Sent,
        kind: WireKind::Reset,
        detail: format!("reset after {token}"),
    };
    let bytes = canon::failure(&error);
    let text = String::from_utf8(bytes).expect("UTF-8");
    assert_eq!(text, "failed sent reset reset after [redacted]");
}

#[test]
fn a_body_over_the_tape_max_is_taped_by_its_digest() {
    let body = vec![b'a'; BODY_TAPE_MAX + 1];
    let bytes = canon::reply(200, &kept_headers(), &body);
    let text = String::from_utf8(bytes.clone()).expect("UTF-8");
    let digest = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&body));
    assert!(text.ends_with(&format!(
        "body-digest sha256:{digest} {}\n\n",
        BODY_TAPE_MAX + 1
    )));
    let refused = canon::decode(&bytes).expect_err("by digest");
    assert_eq!(shape(&refused), replay_stop("body_not_taped"));
    assert!(refused.why.contains(&(BODY_TAPE_MAX + 1).to_string()));
    // Not `retry later`: no tape holds this answer, so the next runs it live.
    assert!(refused.next.contains("--replay"), "{}", refused.next);
    let at_the_max = vec![b'a'; BODY_TAPE_MAX];
    let whole = canon::decode(&canon::reply(200, &[], &at_the_max))
        .expect("whole")
        .expect("an answer");
    assert_eq!(whole.body.len(), BODY_TAPE_MAX);
}

#[test]
fn bytes_reply_did_not_write_are_a_format_error() {
    for bytes in [
        b"HTTP/1.1 200 OK\r\n\r\n".as_slice(),
        b"",
        b"status 200",
        b"status 200\netag x",
        b"status two\n\n",
        b"status +200\n\n",
        b"status 0200\n\n",
        b"status \n\n",
        b"status 200\nnovalue\n\n",
        b"failed later reset x",
        b"failed sent sideways x",
        b"failed sent",
    ] {
        let refused = canon::decode(bytes).expect_err("not a reply");
        assert_eq!(shape(&refused), replay_stop("format"));
        assert!(refused.next.contains("--record"), "{}", refused.next);
    }
}

// ---- The tape ----

/// A header for these tests.
fn header(argv: &[&str]) -> TapeHeader {
    let argv: Vec<String> = argv.iter().map(|a| (*a).to_string()).collect();
    TapeHeader::for_run(&argv, &Endpoints::github_com())
}

/// Records of every kind agh makes, with every test body.
fn every_record() -> Vec<EffectRecord> {
    let request = canon::request(
        &get_with(
            "https://api.github.com/repos/o/r?b=1&a=2",
            &[("Accept", "x")],
        ),
        &Endpoints::github_com(),
    )
    .expect("a request");
    let mut records = vec![
        EffectRecord::Clock(1_760_000_000_123_456_789),
        EffectRecord::Mono(42),
        EffectRecord::Exchange {
            door: DOOR_SLEEP.to_string(),
            request: b"1000000".to_vec(),
            reply: Vec::new(),
        },
    ];
    for body in bodies() {
        records.push(EffectRecord::Exchange {
            door: DOOR_GH.to_string(),
            request: request.clone(),
            reply: canon::reply(200, &kept_headers(), &body),
        });
        records.push(EffectRecord::Emit {
            door: DOOR_STDOUT.to_string(),
            bytes: body.clone(),
        });
        records.push(EffectRecord::Exchange {
            door: "a door=with space".to_string(),
            request: body.clone(),
            reply: body.clone(),
        });
        records.push(EffectRecord::Emit {
            door: "an=emit door".to_string(),
            bytes: body,
        });
    }
    records.push(EffectRecord::Exchange {
        door: DOOR_GH.to_string(),
        request,
        reply: canon::failure(&WireError {
            phase: Phase::NotSent,
            kind: WireKind::Dns,
            detail: "dns: no such host".to_string(),
        }),
    });
    records.push(EffectRecord::Emit {
        door: DOOR_EXIT.to_string(),
        bytes: b"0".to_vec(),
    });
    records
}

#[test]
fn a_tape_reads_back_to_its_records() {
    let h = header(&["release", "list", "o/r", "--jq", ".[] | {a: .b}", "x=y", ""]);
    let records = every_record();
    let text = tape::render(&h, &records);
    assert!(text.starts_with("atape-text 1\nheader\n| agh "));
    assert_eq!(tape::parse(&text).expect("a tape"), (h, records));
}

#[test]
fn a_record_is_written_as_the_subset_writes_it() {
    let sleep = EffectRecord::Exchange {
        door: DOOR_SLEEP.to_string(),
        request: b"2000000000".to_vec(),
        reply: Vec::new(),
    };
    assert_eq!(
        tape::render_record(&sleep).as_deref(),
        Some("exchange sleep\n> 2000000000\n<\n")
    );
    let emit = EffectRecord::Emit {
        door: "a b=c".to_string(),
        bytes: b"x\n\x1b\\".to_vec(),
    };
    assert_eq!(
        tape::render_record(&emit).as_deref(),
        Some("emit a\\x20b\\x3dc\n| x\n| \\x1b\\\\\n")
    );
    assert_eq!(
        tape::render_record(&EffectRecord::Clock(7)).as_deref(),
        Some("clock 7\n")
    );
    assert_eq!(
        tape::render_record(&EffectRecord::Mono(8)).as_deref(),
        Some("mono 8\n")
    );
    assert_eq!(tape::render_record(&EffectRecord::Rand(1)), None);
    assert_eq!(
        tape::render_record(&EffectRecord::Raised {
            door: "stop".to_string(),
            false_reads: 0
        }),
        None
    );
}

#[test]
fn a_header_round_trips() {
    for endpoints in [TapeEndpoints::GithubCom, TapeEndpoints::Loopback] {
        for argv in [
            Vec::new(),
            vec![String::new()],
            vec!["repo".to_string(), "view".to_string(), "o/r".to_string()],
            vec![
                "a b".to_string(),
                "c=d".to_string(),
                "\\n".to_string(),
                "\t".to_string(),
                String::new(),
                "é".to_string(),
            ],
        ] {
            let h = TapeHeader {
                agh_version: "0.0.0+gtest".to_string(),
                api_version: agh::API_VERSION.to_string(),
                argv,
                mode: TapeMode::Anonymous,
                endpoints,
            };
            assert_eq!(TapeHeader::parse(&h.render()).expect("a header"), h);
            let text = tape::render(&h, &[]);
            assert_eq!(tape::parse(&text).expect("a tape"), (h, Vec::new()));
        }
    }
    let h = header(&["repo", "view", "fixture-dev/app"]);
    assert_eq!(h.agh_version, agh::VERSION);
    assert_eq!(h.endpoints, TapeEndpoints::GithubCom);
    assert_eq!(
        h.render().lines().nth(2),
        Some("argv repo view fixture-dev/app")
    );
}

#[test]
fn a_header_agh_cannot_replay_is_refused() {
    let good = header(&["version"]).render();
    let lines: Vec<&str> = good.lines().collect();
    let with = |i: usize, line: &str| {
        let mut changed = lines.clone();
        changed[i] = line;
        changed.join("\n")
    };
    for (text, at) in [
        (with(1, "api 2020-01-01"), 2),
        (with(3, "mode dev"), 4),
        (with(4, "endpoints example.com"), 5),
        (with(0, "aterm 1"), 1),
        (lines[..4].join("\n"), 5),
        (format!("{good}\nextra line"), 6),
        (with(2, "argv a\\q"), 3),
        // More arguments than any exec takes (1 << 20), and a run of extra lines.
        (with(2, &format!("argv{}", " a".repeat((1 << 20) + 1))), 3),
        (format!("{good}{}", "\n".repeat(1000)), 6),
    ] {
        let refused = TapeHeader::parse(&text).expect_err(&text);
        assert_eq!(shape(&refused), replay_stop("format"), "{text}");
        assert!(
            refused.why.starts_with(&format!("line {at}: ")),
            "{text}: {}",
            refused.why
        );
    }
}

#[test]
fn the_parser_refuses_what_is_outside_the_subset() {
    let base = tape::render(&header(&["version"]), &[]);
    // MAGIC, `header` and five header lines: the first item is line 8.
    assert_eq!(base.lines().count(), 7);
    for (tail, at) in [
        ("mono 5 stamp=1,2\n", 8),
        ("note kind\n| a note\n", 8),
        ("mark nested a b\n", 8),
        ("mark truncated 3\n", 8),
        ("end suspended\n", 8),
        ("end returned\n| ok\n", 8),
        ("emit stdout\n| a\n= b\n", 10),
        ("= glue\n", 8),
        ("rand 5\n", 8),
        ("file p\n| bytes\n", 8),
        ("file-err NotFound p\n", 8),
        ("raised stop 0\n", 8),
        ("exchange ctl\n> text\n< ok 0\n!\n|\n", 8),
        ("exchange teller\n> x\n< err\n| y\n", 8),
        ("exchange gh raw\n> a\n< b\n", 8),
        ("emit stdout\n| a\\q\n", 9),
        ("emit stdout\n| a\\x4\n", 9),
        ("exchange gh\n> a\n| b\n", 10),
        ("emit stdout\n> a\n", 9),
        ("emit stdout\n", 9),
        ("mono five\n", 8),
        ("clock 1\n< stray\n", 9),
        ("emit stdout\n|nospace\n", 9),
        ("\n", 8),
        ("mono 1\n\nmono 2\n", 9),
        ("emit ctl\n| x\n", 8),
        ("emit teller\n| x\n", 8),
        ("emit stdout\n| \\x+a\n", 9),
        ("mono +1\n", 8),
        ("clock -1\n", 8),
        ("mono \n", 8),
        ("mono 1 2\n", 8),
        ("exchange gh raw more\n> a\n< b\n", 8),
        ("exchange gh a b stamp=1\n> a\n< b\n", 8),
    ] {
        let text = format!("{base}{tail}");
        let refused = tape::parse(&text).expect_err(tail);
        assert_eq!(shape(&refused), replay_stop("format"), "{tail:?}");
        assert!(
            refused.why.starts_with(&format!("line {at}: ")),
            "{tail:?}: {}",
            refused.why
        );
        // Every replay of it is refused the same: the next names a new tape, never `retry
        // later`.
        assert!(
            refused.next.contains("--record"),
            "{tail:?}: {}",
            refused.next
        );
    }
    let no_magic = base.replacen("atape-text 1\n", "", 1);
    let refused = tape::parse(&no_magic).expect_err("no magic");
    assert_eq!(shape(&refused), replay_stop("format"));
    assert!(refused.why.starts_with("line 1: "));
    let no_header = "atape-text 1\nmono 5\n";
    let refused = tape::parse(no_header).expect_err("no header");
    assert_eq!(shape(&refused), replay_stop("format"));
    assert!(refused.why.starts_with("line 2: "));
    let other_api = base.replace(
        &format!("| api {}\n", agh::API_VERSION),
        "| api 2020-01-01\n",
    );
    let refused = tape::parse(&other_api).expect_err("another api");
    assert_eq!(shape(&refused), replay_stop("format"));
    assert!(refused.why.starts_with("line 4: "), "{}", refused.why);
}

// ---- The sink ----

#[test]
fn a_sink_never_overwrites() {
    let dir = scratch("fx");
    let path = tape_path(dir.path(), "kept");
    std::fs::write(&path, b"not a tape").expect("a file");
    let refused = TapeSink::create(&path, &header(&["version"])).expect_err("exists");
    assert_eq!(
        (refused.kind, refused.sub.clone()),
        (Kind::CouldNotRun, Sub::Local)
    );
    // `--record` never wrote what is in the way: the why names it, and the next names a path
    // only for a tape (which it may remove).
    let named = |e: &agh::Error, path: &Path| e.next.contains(&path.display().to_string());
    assert!(refused.why.contains(&path.display().to_string()));
    assert!(!named(&refused, &path), "{}", refused.next);
    assert_eq!(std::fs::read(&path).expect("the file"), b"not a tape");
    let refused = Fx::record_for_tests(&path, &header(&["version"])).expect_err("exists");
    assert_eq!(
        (refused.kind, refused.sub.clone()),
        (Kind::CouldNotRun, Sub::Local)
    );
    assert_eq!(std::fs::read(&path).expect("the file"), b"not a tape");
    assert!(!named(&refused, &path), "{}", refused.next);
    let a_dir = dir.path().join("a_dir");
    std::fs::create_dir(&a_dir).expect("a directory");
    let refused = TapeSink::create(&a_dir, &header(&["version"])).expect_err("a directory");
    assert_eq!(
        (refused.kind, refused.sub.clone()),
        (Kind::CouldNotRun, Sub::Local)
    );
    assert!(!named(&refused, &a_dir), "{}", refused.next);
    assert!(a_dir.is_dir());
    let taped = tape_path(dir.path(), "taped");
    std::fs::write(&taped, tape::render(&header(&["version"]), &[])).expect("a tape");
    let refused = TapeSink::create(&taped, &header(&["version"])).expect_err("a tape");
    assert!(named(&refused, &taped), "{}", refused.next);
    // A tape at a path with a space: the next names it as one shell word.
    let spaced = dir.path().join("a tape.atape");
    std::fs::write(&spaced, tape::render(&header(&["version"]), &[])).expect("a tape");
    let refused = TapeSink::create(&spaced, &header(&["version"])).expect_err("a tape");
    assert!(
        refused.next.contains(&format!("'{}'", spaced.display())),
        "{}",
        refused.next
    );
    let missing_dir = dir.path().join("no-such-dir").join("t.atape");
    let refused = TapeSink::create(&missing_dir, &header(&["version"])).expect_err("no dir");
    assert_eq!(
        (refused.kind, refused.sub.clone()),
        (Kind::CouldNotRun, Sub::Local)
    );
}

#[cfg(unix)]
#[test]
fn a_tape_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("fx");
    let path = tape_path(dir.path(), "private");
    let sink = TapeSink::create(&path, &header(&["version"])).expect("a sink");
    sink.finish().expect("finished");
    let mode = std::fs::metadata(&path)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

/// A directory made readable again (0700) when this drops, a panic included, so the scratch
/// directory's own drop can list and remove it.
#[cfg(unix)]
struct Readable<'a>(&'a Path);

#[cfg(unix)]
impl Drop for Readable<'_> {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o700));
    }
}

// Opening a FIFO waits for a writer, so the refusal for one at the path must not open it.
#[cfg(unix)]
#[test]
fn a_sink_refuses_a_fifo_without_opening_it() {
    use std::os::unix::fs::FileTypeExt;
    let dir = scratch("fx");
    let fifo = tape_path(dir.path(), "fifo");
    let made = aterm_uds::spawnfd::command("mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo");
    assert!(made.success(), "mkfifo {}", fifo.display());
    let (tx, rx) = mpsc::channel();
    let at = fifo.clone();
    std::thread::spawn(move || {
        let _ = tx.send(TapeSink::create(&at, &header(&["version"])).map(|_| ()));
    });
    let refused = rx
        .recv_timeout(HANG)
        .expect("the refusal came without a writer")
        .expect_err("exists");
    assert_eq!(
        (refused.kind, refused.sub.clone()),
        (Kind::CouldNotRun, Sub::Local)
    );
    assert!(!refused.next.contains(&fifo.display().to_string()));
    let kind = std::fs::symlink_metadata(&fifo)
        .expect("the FIFO")
        .file_type();
    assert!(kind.is_fifo());
}

#[cfg(unix)]
#[test]
fn a_directory_that_cannot_be_flushed_keeps_the_whole_tape() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("fx");
    // Write and search, no read (the shape of a drop box): the file can be made and read by
    // its path, but the directory cannot be opened to flush its entry.
    let sealed = dir.path().join("write-only");
    std::fs::create_dir(&sealed).expect("the parent");
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o300))
        .expect("write and search only");
    let _readable = Readable(&sealed);
    if std::fs::read_dir(&sealed).is_ok() {
        // A user the mode does not bind (root): no directory here fails to flush.
        return;
    }
    let path = sealed.join("new.atape");
    let fx = Fx::record_for_tests(&path, &header(&["version"])).expect("a recording");
    fx.emit_stdout(b"v1\n").expect("stdout");
    let refused = fx.finish(0).expect_err("the directory is not flushed");
    assert_eq!(
        (refused.kind, refused.sub.clone()),
        (Kind::CouldNotRun, Sub::Local)
    );
    // The next names the tape, which is kept and replays.
    assert!(
        refused.next.contains(&path.display().to_string()),
        "{}",
        refused.next
    );
    let replayed = Fx::replay_for_tests(&path).map(|(fx, _)| {
        let out = fx.emit_stdout(b"v1\n");
        (out, fx.finish(0))
    });
    let (out, finished) = replayed.expect("the tape that is left replays");
    out.expect("its stdout");
    finished.expect("its exit");
}

#[test]
fn a_sink_redacts_each_record_and_latches_an_unknown_kind() {
    let dir = scratch("fx");
    let path = tape_path(dir.path(), "redacted");
    let token = agh_fake::token("t");
    let sink = TapeSink::create(&path, &header(&["version"])).expect("a sink");
    sink.append(&EffectRecord::Emit {
        door: DOOR_STDOUT.to_string(),
        bytes: format!("the token {token} leaked\n").into_bytes(),
    });
    assert_eq!(sink.failed(), None);
    sink.finish().expect("finished");
    let text = std::fs::read_to_string(&path).expect("the tape");
    assert!(!text.contains(&token));
    assert!(text.contains("| the token [redacted] leaked\n"));
    let (_, records) = tape::parse(&text).expect("a tape");
    assert_eq!(
        records,
        [EffectRecord::Emit {
            door: DOOR_STDOUT.to_string(),
            bytes: b"the token [redacted] leaked\n".to_vec(),
        }]
    );

    let path = tape_path(dir.path(), "rand");
    let sink = TapeSink::create(&path, &header(&["version"])).expect("a sink");
    sink.append(&EffectRecord::Rand(9));
    sink.append(&EffectRecord::Mono(1));
    assert!(sink.failed().is_some_and(|why| why.contains("Rand")));
    let refused = sink.finish().expect_err("latched");
    assert_eq!(
        (refused.kind, refused.sub.clone()),
        (Kind::CouldNotRun, Sub::Local)
    );
    assert!(!path.exists());
}

#[test]
fn a_sink_redacts_the_raw_bytes_never_their_escaped_rendering() {
    let dir = scratch("fx");
    let path = tape_path(dir.path(), "forms");
    // Each form the shapes catch in raw text, which the tape's escape would hide from them:
    // a JSON object quoted inside a JSON string (`\"`), and a quoted bearer value.
    let forms: [(&str, &[u8]); 2] = [
        (
            "SEKRITONE1",
            br#"{"log":"{\"Authorization\":\"token SEKRITONE1\"}"}"#,
        ),
        ("SEKRITTHREE3", br#"{"m":"bearer \"SEKRITTHREE3\""}"#),
    ];
    let sink = TapeSink::create(&path, &header(&["api", "search"])).expect("a sink");
    for (_, bytes) in forms {
        sink.append(&EffectRecord::Exchange {
            door: DOOR_GH.to_string(),
            request: b"GET /x".to_vec(),
            reply: canon::reply(200, &[], bytes),
        });
        sink.append(&EffectRecord::Emit {
            door: DOOR_STDOUT.to_string(),
            bytes: bytes.to_vec(),
        });
    }
    sink.finish().expect("finished");
    let text = std::fs::read_to_string(&path).expect("the tape");
    for (secret, _) in forms {
        assert!(!text.contains(secret), "{secret} is on disk:\n{text}");
    }
    assert_eq!(agh::redact::Redactor::shapes(&text), text);
    let (taped, records) = tape::parse(&text).expect("a tape");
    assert_eq!(taped.argv.len(), 2);
    assert_eq!(records.len(), 4);
    // Every JSON body the sink redacted is still JSON.
    for record in &records {
        if let EffectRecord::Emit { bytes, .. } = record
            && bytes.starts_with(b"{")
        {
            let parsed: Result<aterm_json::Value, _> = aterm_json::from_slice(bytes);
            assert!(parsed.is_ok(), "{}", String::from_utf8_lossy(bytes));
        }
    }
}

#[test]
fn a_sink_writes_no_line_the_shapes_would_still_change() {
    let dir = scratch("fx");
    // Raw bytes the shapes leave (a run cut short by a CR, or too short for rule 3), or
    // redact, whose escaped rendering they would change: the escape writes a CR `\r` and a
    // control byte `\xHH` (a form feed among them), each longer than the byte and neither a
    // blank. (A tab's `\t` is a blank to the shapes, so a tab's rendering is not among them.)
    let records = [
        EffectRecord::Emit {
            door: DOOR_STDOUT.to_string(),
            bytes: b"OAuth uses Bearer tokens\r\n".to_vec(),
        },
        EffectRecord::Emit {
            door: DOOR_STDOUT.to_string(),
            bytes: b"Bearer abcd\x01\n".to_vec(),
        },
        EffectRecord::Exchange {
            door: DOOR_GH.to_string(),
            request: b"GET /x".to_vec(),
            reply: canon::reply(200, &[], b"Authorization:\x0ctoken SEKRITTWO2\n"),
        },
    ];
    for (i, record) in records.iter().enumerate() {
        let path = tape_path(dir.path(), &format!("shaped-{i}"));
        let sink = TapeSink::create(&path, &header(&["api", "/x"])).expect("a sink");
        sink.append(&EffectRecord::Mono(1));
        sink.append(record);
        sink.append(&EffectRecord::Mono(2));
        assert!(sink.failed().is_some(), "{record:?}");
        let text = std::fs::read_to_string(&path).expect("the tape");
        assert!(!text.contains("SEKRITTWO2"), "{text}");
        assert_eq!(agh::redact::Redactor::shapes(&text), text);
        assert_eq!(
            tape::parse(&text).expect("a tape").1,
            [EffectRecord::Mono(1)],
            "{record:?}"
        );
        let refused = sink.finish().expect_err("latched");
        assert_eq!(
            (refused.kind, refused.sub.clone()),
            (Kind::CouldNotRun, Sub::Local)
        );
        // Every run would be refused the same: the file, which holds no exit, is gone, so a
        // second `--record` to the path is not refused for a file that exists.
        assert!(!path.exists(), "{record:?}");
        assert!(refused.next.contains("--record"), "{}", refused.next);
        assert_eq!(sink.finish().expect_err("still latched"), refused);
    }

    // A tab after the header is a blank raw and escaped, so the record is written, redacted.
    let path = tape_path(dir.path(), "shaped-tab");
    let sink = TapeSink::create(&path, &header(&["api", "/x"])).expect("a sink");
    sink.append(&EffectRecord::Exchange {
        door: DOOR_GH.to_string(),
        request: b"GET /x".to_vec(),
        reply: canon::reply(200, &[], b"Authorization:\ttoken SEKRITTHREE\n"),
    });
    sink.finish()
        .expect("a tab's rendering is one the shapes leave");
    let text = std::fs::read_to_string(&path).expect("the tape");
    assert!(!text.contains("SEKRITTHREE"), "{text}");
    assert_eq!(agh::redact::Redactor::shapes(&text), text);

    // A header the same: arguments that read as a shape once joined, or once their spaces
    // are written `\x20`. No file is made.
    for argv in [
        vec!["api", "Bearer", "abcdefghij"],
        vec!["api", "search", "q=Authorization: token SEKRITARG1"],
    ] {
        let path = tape_path(dir.path(), "shaped-head");
        let refused = TapeSink::create(&path, &header(&argv)).expect_err("a shaped header");
        assert_eq!(
            (refused.kind, refused.sub.clone()),
            (Kind::CouldNotRun, Sub::Local),
            "{argv:?}"
        );
        assert!(!path.exists(), "{argv:?}");
    }
}

#[test]
fn a_finished_sink_writes_no_record_after_the_exit() {
    let dir = scratch("fx");
    let path = tape_path(dir.path(), "finished");
    let sink = TapeSink::create(&path, &header(&["version"])).expect("a sink");
    sink.append(&EffectRecord::Mono(1));
    sink.finish().expect("finished");
    assert_eq!(sink.failed(), None);
    sink.append(&EffectRecord::Mono(2));
    assert!(sink.failed().is_some());
    let refused = sink.finish().expect_err("a record after the exit");
    assert_eq!(
        (refused.kind, refused.sub.clone()),
        (Kind::CouldNotRun, Sub::Local)
    );
    // The tape is whole, its exit its last record: it is kept, and the next names it.
    assert!(refused.next.contains(&path.display().to_string()));
    let text = std::fs::read_to_string(&path).expect("the tape");
    assert_eq!(
        tape::parse(&text).expect("a tape").1,
        [EffectRecord::Mono(1)]
    );
}

#[test]
fn a_sink_never_writes_a_tape_a_replay_would_refuse_for_its_size() {
    let dir = scratch("fx");
    let header = header(&["release", "list", "o/r"]);
    let head = tape::render(&header, &[]);
    let header_size = u64::try_from(head.len()).expect("a header fits u64");
    let short = tape_path(dir.path(), "short-header");
    let refused = match TapeSink::create_within(&short, &header, header_size - 1) {
        Ok(_) => panic!("a header larger than the bound must be refused"),
        Err(error) => error,
    };
    assert_eq!(
        (refused.kind, refused.sub.clone()),
        (Kind::CouldNotRun, Sub::Local)
    );
    assert!(!short.exists(), "refused before creating the file");

    let exact = tape_path(dir.path(), "exact-header");
    let sink =
        TapeSink::create_within(&exact, &header, header_size).expect("the header fits exactly");
    assert_eq!(std::fs::read(&exact).expect("the tape"), head.as_bytes());
    assert_eq!(sink.failed(), None);
    sink.finish().expect("the header at the bound is durable");
    assert_eq!(
        tape::parse(&std::fs::read_to_string(&exact).expect("the tape")).expect("a tape"),
        (header.clone(), Vec::new())
    );

    let path = tape_path(dir.path(), "big");
    // The bound a test can reach in kilobytes (`TapeSink::create_within`; `create`'s is
    // `TAPE_MAX`, the most a replay reads).
    const MAX: u64 = 16 << 10;
    let sink = TapeSink::create_within(&path, &header, MAX).expect("a sink");
    sink.append(&EffectRecord::Mono(1));
    // Each control byte is written `\x01`, four bytes: this one record would take the file
    // past the bound, though its own bytes are a quarter of it.
    let quarter = usize::try_from(MAX / 4).expect("fits") + 1;
    sink.append(&EffectRecord::Emit {
        door: DOOR_STDOUT.to_string(),
        bytes: vec![1; quarter],
    });
    assert!(sink.failed().is_some());
    sink.append(&EffectRecord::Mono(2));
    let size = std::fs::metadata(&path).expect("the tape").len();
    assert!(size <= MAX, "{size}");
    let text = std::fs::read_to_string(&path).expect("the tape");
    assert_eq!(
        tape::parse(&text).expect("a tape").1,
        [EffectRecord::Mono(1)]
    );
    let refused = sink.finish().expect_err("latched");
    assert_eq!(
        (refused.kind, refused.sub.clone()),
        (Kind::CouldNotRun, Sub::Local)
    );
    // Every run would be refused the same, and the file holds no exit: it is gone.
    assert!(!path.exists());
    assert!(refused.next.contains("--record"), "{}", refused.next);
}

#[test]
fn a_recording_whose_output_no_tape_can_hold_leaves_no_file() {
    let dir = scratch("fx");
    let path = tape_path(dir.path(), "cr");
    // Plain stdout whose rendered `\r` makes the run after `Bearer ` long enough to read as a
    // shape: every run of it is refused the same.
    let stdout: &[u8] = b"OAuth uses Bearer tokens\r\n";
    for attempt in 0..2 {
        let recording =
            Fx::record_for_tests(&path, &header(&["api", "/x"])).expect("the path is free");
        recording.mono().expect("mono");
        recording.emit_stdout(stdout).expect("the live run prints");
        let refused = recording.finish(0).expect_err("no tape can hold it");
        assert_eq!(
            (refused.kind, refused.sub.clone()),
            (Kind::CouldNotRun, Sub::Local),
            "{attempt}"
        );
        assert!(refused.next.contains("--record"), "{}", refused.next);
        assert!(
            !path.exists(),
            "{attempt}: a tape that could never replay was left"
        );
    }
}

#[test]
fn a_rerecording_whose_output_no_tape_can_hold_leaves_no_new_tape() {
    let dir = scratch("fx");
    let old = tape_path(dir.path(), "old");
    let new = tape_path(dir.path(), "new");
    let old_text = tape::render(&header(&["api", "/x"]), &[EffectRecord::Mono(1)]);
    std::fs::write(&old, &old_text).expect("the old tape");
    let (fx, _) = Fx::rerecord_for_tests(&old, &new).expect("a rerecording");
    fx.mono().expect("mono");
    fx.emit_stdout(b"OAuth uses Bearer tokens\r\n")
        .expect("the run prints");
    let refused = fx.finish(0).expect_err("no tape can hold it");
    assert_eq!(
        (refused.kind, refused.sub.clone()),
        (Kind::CouldNotRun, Sub::Local)
    );
    assert!(!new.exists());
    assert_eq!(
        std::fs::read_to_string(&old).expect("the old tape"),
        old_text
    );
}

// ---- Recording, then replaying ----

/// The two answers the record and replay test is scripted with.
fn two_answers(transport: &ScriptedTransport) {
    transport.answer(
        200,
        &[
            ("ETag", "\"a\""),
            ("X-Dropped", "d"),
            ("Link", "<https://api.github.com/x?page=2>; rel=\"next\""),
            ("X-RateLimit-Remaining", "59"),
        ],
        b"[{\"id\":1}]\n",
    );
    transport.answer(
        404,
        &[("Content-Type", "application/json")],
        b"{\"message\":\"Not Found\"}",
    );
}

/// An answer as status, headers and body.
type Parts = (u16, Vec<(String, String)>, Vec<u8>);

/// What one driven run saw: the clocks, the two answers.
#[derive(Debug, PartialEq, Eq)]
struct Seen {
    mono: Duration,
    wall: u64,
    answers: Vec<Parts>,
}

/// An answer's parts, its body read.
fn parts(answer: Result<WireResponse, WireError>) -> Parts {
    let mut answer = answer.expect("an answer");
    let mut body = Vec::new();
    answer.body.read_to_end(&mut body).expect("the body");
    (answer.status, answer.headers, body)
}

/// Drive `fx` through every door: mono, wall_secs, sleep(1 ms), two `gh`, `emit_stdout` and
/// `finish(0)`.
fn drive(fx: &Fx, transport: &ScriptedTransport) -> Seen {
    let e = Endpoints::github_com();
    let mono = fx.mono().expect("mono");
    let wall = fx.wall_secs().expect("wall");
    fx.sleep(Duration::from_millis(1)).expect("sleep");
    let first = fx
        .gh_for_tests(
            transport,
            &e,
            get_with(
                "https://api.github.com/repos/o/r/releases?per_page=100",
                &[("Accept", "a")],
            ),
        )
        .expect("no divergence");
    let second = fx
        .gh_for_tests(
            transport,
            &e,
            get_with("https://api.github.com/repos/o/r/releases/tags/v9", &[]),
        )
        .expect("no divergence");
    fx.emit_stdout(b"[{\"id\":1}]\n").expect("emit");
    fx.finish(0).expect("finish");
    Seen {
        mono,
        wall,
        answers: vec![parts(first), parts(second)],
    }
}

#[test]
fn a_recorded_run_replays_offline_to_the_same_answers() {
    let dir = scratch("fx");
    let path = tape_path(dir.path(), "run");
    let h = header(&["release", "list", "o/r"]);
    let live = ScriptedTransport::new();
    two_answers(&live);
    let recording = Fx::record_for_tests(&path, &h).expect("a recording");
    assert_eq!(recording.mode(), Mode::Recording);
    let recorded = drive(&recording, &live);
    assert_eq!(live.remaining(), 0);
    assert_eq!(live.requests().len(), 2);
    assert!(recorded.wall > NEW_YEAR_2026);
    assert_eq!(
        recorded.answers[0].1,
        [
            ("etag".to_string(), "\"a\"".to_string()),
            (
                "link".to_string(),
                "<https://api.github.com/x?page=2>; rel=\"next\"".to_string()
            ),
            ("x-ratelimit-remaining".to_string(), "59".to_string()),
        ]
    );

    let offline = ScriptedTransport::new();
    offline.answer(500, &[], b"never read");
    let (replaying, taped) = Fx::replay_for_tests(&path).expect("a replay");
    assert_eq!(replaying.mode(), Mode::Replaying);
    assert_eq!(taped, h);
    let replayed = drive(&replaying, &offline);
    assert_eq!(replayed, recorded);
    assert_eq!(offline.remaining(), 1);
    assert!(offline.requests().is_empty());
    // That a replayed sleep takes no time is held by `a_replayed_hour_takes_no_real_time`:
    // `slept` lists simulated sleeps alone, so it is empty here whatever the replay did.
}

#[test]
fn a_recording_never_tapes_an_answer_that_holds_a_token_shape() {
    let dir = scratch("fx");
    let e = Endpoints::github_com();
    // Each body a replay could not answer as the run saw it: a quoted credential the raw
    // shapes rewrite (its decoded `--jq .body` would render other stdout than its replay),
    // and two that only the decoded JSON string shows, behind a `\t` and a `\u0009` escape.
    let bodies: [&[u8]; 3] = [
        br#"{"body":"curl -H \"Authorization: Bearer SEKRITVALUE\" https://api.github.com"}"#,
        br#"{"body":"Bearer\tSEKRITVALUE"}"#,
        br#"{"body":"Bearer\u0009SEKRITVALUE"}"#,
    ];
    for (n, body) in bodies.into_iter().enumerate() {
        let path = tape_path(dir.path(), &format!("shaped-{n}"));
        let live = ScriptedTransport::new();
        live.answer(200, &[], body);
        let recording =
            Fx::record_for_tests(&path, &header(&["api", "/user"])).expect("a recording");
        let refused = recording
            .gh_for_tests(&live, &e, get(1 << 20))
            .expect_err("not taped");
        assert_eq!(shape(&refused), replay_stop("body_not_taped"), "{n}");
        assert!(refused.next.contains("--record"), "{n}: {}", refused.next);
        assert_eq!(&recording.emit_stdout(body).expect_err("latched"), &refused);
        assert_eq!(&recording.finish(3).expect_err("latched"), &refused);
        assert!(!path.exists(), "{n}: a tape that could not replay was left");
    }
}

#[test]
fn a_recording_never_sends_a_question_that_holds_a_token_shape() {
    let dir = scratch("fx");
    let path = tape_path(dir.path(), "asked");
    let live = ScriptedTransport::new();
    live.answer(200, &[], b"{}");
    let recording = Fx::record_for_tests(&path, &header(&["api", "/x"])).expect("a recording");
    let url = format!(
        "https://api.github.com/repos/fixture-dev/{}",
        agh_fake::token("path")
    );
    let refused = recording
        .gh_for_tests(&live, &Endpoints::github_com(), get_with(&url, &[]))
        .expect_err("not sent");
    assert_eq!(shape(&refused), replay_stop("stream_not_taped"));
    assert!(live.requests().is_empty());
    assert!(refused.next.contains("--record"), "{}", refused.next);
    assert_eq!(&recording.finish(3).expect_err("latched"), &refused);
    assert!(!path.exists());
}

#[test]
fn stdout_that_shows_a_token_shape_replays_as_it_was_recorded() {
    let dir = scratch("fx");
    let path = tape_path(dir.path(), "rendered");
    let e = Endpoints::github_com();
    // No string of this answer holds a shape, but a rendering that joins two of its fields
    // with a tab does: the tape holds that stdout shaped, and the replay's is compared so.
    let body = br#"{"a":"Bearer","b":"SEKRITVALUE9"}"#;
    let rendered = b"Bearer\tSEKRITVALUE9\n";
    let live = ScriptedTransport::new();
    live.answer(200, &[], body);
    let recording = Fx::record_for_tests(&path, &header(&["api", "/user"])).expect("a recording");
    let answer = recording
        .gh_for_tests(&live, &e, get(1 << 20))
        .expect("taped");
    assert_eq!(parts(answer).2, body);
    recording.emit_stdout(rendered).expect("emit");
    recording.finish(0).expect("finish");
    let text = std::fs::read_to_string(&path).expect("the tape");
    assert!(!text.contains("| Bearer\\tSEKRITVALUE9"), "{text}");

    let (replaying, _) = Fx::replay_for_tests(&path).expect("a replay");
    let offline = ScriptedTransport::new();
    let answer = replaying
        .gh_for_tests(&offline, &e, get(1 << 20))
        .expect("replayed");
    assert_eq!(parts(answer).2, body);
    replaying.emit_stdout(rendered).expect("the same stdout");
    replaying.finish(0).expect("finish");
    assert!(offline.requests().is_empty());
}

#[test]
fn a_replayed_answer_holds_the_kept_headers_alone() {
    let dir = scratch("fx");
    let e = Endpoints::github_com();
    // A tape this build did not write: its reply holds a header no live run keeps.
    let reply = canon::reply(
        200,
        &[
            ("X-Dropped".to_string(), "d".to_string()),
            ("ETag".to_string(), "\"e\"".to_string()),
        ],
        b"{}",
    );
    let exchange = EffectRecord::Exchange {
        door: DOOR_GH.to_string(),
        request: canon::request(&get(1 << 20), &e).expect("a request"),
        reply,
    };
    let fx = replaying(dir.path(), "headers", std::slice::from_ref(&exchange));
    let answer = fx
        .gh_for_tests(&ScriptedTransport::new(), &e, get(1 << 20))
        .expect("replayed");
    assert_eq!(parts(answer).1, [("etag".to_string(), "\"e\"".to_string())]);

    let old = tape_path(dir.path(), "old-headers");
    let new = tape_path(dir.path(), "new-headers");
    std::fs::write(&old, tape::render(&header(&["x"]), &[exchange])).expect("the old tape");
    let (fx, _) = Fx::rerecord_for_tests(&old, &new).expect("a rerecording");
    let answer = fx
        .gh_for_tests(&ScriptedTransport::new(), &e, get(1 << 20))
        .expect("replayed");
    assert_eq!(parts(answer).1, [("etag".to_string(), "\"e\"".to_string())]);
}

#[test]
fn a_recorded_wire_failure_replays_as_the_same_failure() {
    let dir = scratch("fx");
    let path = tape_path(dir.path(), "failure");
    let failure = WireError {
        phase: Phase::Sent,
        kind: WireKind::Idle,
        detail: "no bytes for 60s".to_string(),
    };
    let live = ScriptedTransport::new();
    live.push(Scripted::Fail(failure.clone()));
    live.answer(200, &[], b"0123456789");
    let e = Endpoints::github_com();
    let recording = Fx::record_for_tests(&path, &header(&["api", "/x"])).expect("a recording");
    let first = recording
        .gh_for_tests(&live, &e, get(1 << 20))
        .expect("taped");
    assert_eq!(first.expect_err("the failure"), failure);
    // Over the limit: the head, the body cut one byte past it, taped as it was handed on.
    let cut = parts(recording.gh_for_tests(&live, &e, get(4)).expect("taped"));
    assert_eq!(cut.2, b"01234");
    recording.finish(1).expect("finish");

    let (replaying, _) = Fx::replay_for_tests(&path).expect("a replay");
    let offline = ScriptedTransport::new();
    let again = replaying
        .gh_for_tests(&offline, &e, get(1 << 20))
        .expect("replayed");
    assert_eq!(again.expect_err("the failure"), failure);
    let again = replaying
        .gh_for_tests(&offline, &e, get(4))
        .expect("replayed");
    assert_eq!(parts(again), cut);
    replaying.finish(1).expect("finish");
    assert!(offline.requests().is_empty());
}

// A cut is `req.limit + 1` bytes; at a limit of `BODY_TAPE_MAX` it is one byte past what a
// tape holds whole, so it is taped by its digest and its replay refuses by name (16.12).
#[test]
fn a_cut_past_the_tape_max_is_taped_by_its_digest() {
    let dir = scratch("fx");
    let path = tape_path(dir.path(), "cut-by-digest");
    let live = ScriptedTransport::new();
    live.answer(200, &[], &vec![b'a'; BODY_TAPE_MAX + 10]);
    let e = Endpoints::github_com();
    let limit = u64::try_from(BODY_TAPE_MAX).expect("a u64");
    let recording = Fx::record_for_tests(&path, &header(&["api", "/x"])).expect("a recording");
    let cut = parts(
        recording
            .gh_for_tests(&live, &e, get(limit))
            .expect("taped"),
    );
    assert_eq!(cut.2.len(), BODY_TAPE_MAX + 1);
    recording.finish(0).expect("finish");

    let (replaying, _) = Fx::replay_for_tests(&path).expect("a replay");
    let offline = ScriptedTransport::new();
    let refused = replaying
        .gh_for_tests(&offline, &e, get(limit))
        .expect_err("by digest");
    assert_eq!(refused.sub, Sub::Replay(Replay::BodyNotTaped));
    assert!(offline.requests().is_empty());
}

#[test]
fn a_tape_refuses_a_stream_and_a_body_before_anything_is_sent() {
    let dir = scratch("fx");
    let path = tape_path(dir.path(), "refused");
    let live = ScriptedTransport::new();
    live.answer(200, &[], b"x");
    let recording = Fx::record_for_tests(&path, &header(&["api", "/x"])).expect("a recording");
    let refused = recording
        .gh_stream_for_tests(&live, get(1))
        .expect_err("streamed");
    assert_eq!(shape(&refused), replay_stop("stream_not_taped"));
    assert!(refused.next.contains("--record"), "{}", refused.next);
    // The refusal is latched: every later door answers it.
    let later = recording.mono().expect_err("latched");
    assert_eq!(later, refused);
    assert!(live.requests().is_empty());

    let path = tape_path(dir.path(), "body");
    let recording = Fx::record_for_tests(&path, &header(&["api", "/x"])).expect("a recording");
    let mut post = get(1 << 20);
    post.method = Method::Post;
    post.body = WireBody::Bytes(b"{}");
    let refused = recording
        .gh_for_tests(&live, &Endpoints::github_com(), post)
        .expect_err("a body");
    assert_eq!(shape(&refused), replay_stop("stream_not_taped"));
    assert!(refused.next.contains("--record"), "{}", refused.next);
    assert!(live.requests().is_empty());
}

#[test]
fn a_replayed_hour_takes_no_real_time() {
    let dir = scratch("fx");
    let path = tape_path(dir.path(), "hour");
    let hour = Duration::from_secs(3600);
    let records = vec![EffectRecord::Exchange {
        door: DOOR_SLEEP.to_string(),
        request: hour.as_nanos().to_string().into_bytes(),
        reply: Vec::new(),
    }];
    std::fs::write(&path, tape::render(&header(&["x"]), &records)).expect("a tape");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let slept = Fx::replay_for_tests(&path).map(|(fx, _)| fx.sleep(hour).is_ok());
        let _ = tx.send(slept.ok());
    });
    let slept = rx.recv_timeout(HANG).expect("the hour passed at once");
    assert_eq!(slept, Some(true));
}

// ---- Divergences ----

/// A replaying Fx over a tape of `records`, kept in `dir`.
fn replaying(dir: &Path, label: &str, records: &[EffectRecord]) -> Fx {
    let path = tape_path(dir, label);
    std::fs::write(&path, tape::render(&header(&["x"]), records)).expect("a tape");
    Fx::replay_for_tests(&path).expect("a replay").0
}

/// The taped exchange of a GET of `path` on the API, answered 200 with `body`.
fn gh_exchange(path: &str, body: &[u8]) -> EffectRecord {
    let req = get_with(&format!("https://api.github.com{path}"), &[]);
    EffectRecord::Exchange {
        door: DOOR_GH.to_string(),
        request: canon::request(&req, &Endpoints::github_com()).expect("a request"),
        reply: canon::reply(200, &[], body),
    }
}

/// A seam divergence's next is a live record: replaying or rerecording the tape again would
/// meet the same divergence.
fn points_live(first: &agh::Error) {
    assert!(
        first.next.contains("--record") && !first.next.contains("--replay"),
        "{}",
        first.next
    );
}

/// Every door of `fx` answers `first`, without reading the tape.
fn every_door_answers(fx: &Fx, first: &agh::Error) {
    let transport = ScriptedTransport::new();
    let e = Endpoints::github_com();
    assert_eq!(&fx.mono().expect_err("mono"), first);
    assert_eq!(&fx.wall_secs().expect_err("wall"), first);
    assert_eq!(&fx.sleep(Duration::from_secs(1)).expect_err("sleep"), first);
    assert_eq!(
        &fx.gh_for_tests(&transport, &e, get(1 << 20))
            .expect_err("gh"),
        first
    );
    assert_eq!(
        &fx.gh_stream_for_tests(&transport, get(1))
            .expect_err("stream"),
        first
    );
    assert_eq!(&fx.emit_stdout(b"x").expect_err("emit"), first);
    assert_eq!(&fx.finish(0).expect_err("finish"), first);
    assert!(transport.requests().is_empty());
}

#[test]
fn each_divergence_is_typed_and_latched() {
    let dir = scratch("fx");
    let transport = ScriptedTransport::new();
    let e = Endpoints::github_com();

    let fx = replaying(dir.path(), "exhausted", &[EffectRecord::Mono(1)]);
    fx.mono().expect("the taped mono");
    let first = fx.mono().expect_err("past the end");
    assert_eq!(shape(&first), replay_stop("exhausted"));
    points_live(&first);
    every_door_answers(&fx, &first);

    let fx = replaying(dir.path(), "kind", &[EffectRecord::Clock(1)]);
    let first = fx.mono().expect_err("a clock");
    assert_eq!(shape(&first), replay_stop("kind"));
    points_live(&first);
    every_door_answers(&fx, &first);

    let fx = replaying(dir.path(), "door", &[gh_exchange("/x", b"")]);
    let first = fx.sleep(Duration::from_millis(1)).expect_err("a gh");
    assert_eq!(shape(&first), replay_stop("door"));
    points_live(&first);
    every_door_answers(&fx, &first);

    let fx = replaying(dir.path(), "request", &[gh_exchange("/repos/o/app", b"{}")]);
    let first = fx
        .gh_for_tests(
            &transport,
            &e,
            get_with("https://api.github.com/repos/o/apq", &[]),
        )
        .expect_err("another path");
    assert_eq!(shape(&first), replay_stop("request"));
    // plan §12a.3: a request divergence's next re-records the tape, as `Error::replay` names it.
    assert!(first.next.contains("--replay") && first.next.contains("--record"));
    every_door_answers(&fx, &first);

    let stdout = EffectRecord::Emit {
        door: DOOR_STDOUT.to_string(),
        bytes: b"taped\n".to_vec(),
    };
    let fx = replaying(dir.path(), "output", &[stdout]);
    let first = fx.emit_stdout(b"other\n").expect_err("other bytes");
    assert_eq!(shape(&first), replay_stop("output"));
    // A rerecording writes this run's stdout, so the output's next re-records the tape.
    assert!(first.next.contains("--replay") && first.next.contains("--record"));
    every_door_answers(&fx, &first);

    let exit = EffectRecord::Emit {
        door: DOOR_EXIT.to_string(),
        bytes: b"0".to_vec(),
    };
    let fx = replaying(dir.path(), "exit", std::slice::from_ref(&exit));
    let first = fx.finish(1).expect_err("another exit");
    assert_eq!(shape(&first), replay_stop("output"));
    every_door_answers(&fx, &first);

    let fx = replaying(
        dir.path(),
        "unconsumed",
        &[exit, EffectRecord::Mono(1), EffectRecord::Clock(2)],
    );
    let first = fx.finish(0).expect_err("records left");
    assert_eq!(shape(&first), replay_stop("unconsumed"));
    assert!(first.next.contains("--replay") && first.next.contains("--record"));
    assert!(
        first.why.split_whitespace().any(|w| w == "2"),
        "{}",
        first.why
    );
    every_door_answers(&fx, &first);

    let by_digest = EffectRecord::Exchange {
        door: DOOR_GH.to_string(),
        request: canon::request(&get(1 << 20), &e).expect("a request"),
        reply: canon::reply(200, &[], &vec![b'a'; BODY_TAPE_MAX + 1]),
    };
    let fx = replaying(dir.path(), "digest", &[by_digest]);
    let first = fx
        .gh_for_tests(&transport, &e, get(1 << 20))
        .expect_err("by digest");
    assert_eq!(shape(&first), replay_stop("body_not_taped"));
    assert!(first.next.contains("--replay"), "{}", first.next);
    every_door_answers(&fx, &first);
    assert!(transport.requests().is_empty());
}

#[test]
fn a_tape_that_cannot_be_read_is_named() {
    let dir = scratch("fx");
    let missing = tape_path(dir.path(), "missing");
    let refused = Fx::replay_for_tests(&missing).expect_err("missing");
    assert_eq!(
        (refused.kind, refused.sub.clone()),
        (Kind::CouldNotRun, Sub::Local)
    );
    let garbage = tape_path(dir.path(), "garbage");
    std::fs::write(&garbage, [0xff, 0xfe, 0x00]).expect("a file");
    let refused = Fx::replay_for_tests(&garbage).expect_err("not text");
    assert_eq!(refused.sub, Sub::Replay(Replay::Format));
    assert!(refused.next.contains("--record"), "{}", refused.next);
}

// ---- Rerecording ----

#[test]
fn a_rerecording_keeps_the_old_exchanges_and_takes_this_runs_output() {
    let dir = scratch("fx");
    let old = tape_path(dir.path(), "old");
    let new = tape_path(dir.path(), "new");
    let mut h = header(&["release", "view", "o/r", "v1"]);
    h.agh_version = "0.0.1+gold".to_string();
    let exchange = gh_exchange("/repos/o/r/releases/tags/v1", b"{\"tag_name\":\"v1\"}\n");
    let old_records = vec![
        EffectRecord::Mono(7),
        EffectRecord::Clock(1_760_000_000_000_000_000),
        exchange.clone(),
        EffectRecord::Exchange {
            door: DOOR_SLEEP.to_string(),
            request: b"5".to_vec(),
            reply: Vec::new(),
        },
        EffectRecord::Emit {
            door: DOOR_STDOUT.to_string(),
            bytes: b"old output\n".to_vec(),
        },
        EffectRecord::Emit {
            door: DOOR_EXIT.to_string(),
            bytes: b"0".to_vec(),
        },
    ];
    let old_text = tape::render(&h, &old_records);
    std::fs::write(&old, &old_text).expect("the old tape");

    let (fx, taped) = Fx::rerecord_for_tests(&old, &new).expect("a rerecording");
    assert_eq!(fx.mode(), Mode::Rerecording);
    assert_eq!(taped, h);
    let transport = ScriptedTransport::new();
    assert_eq!(fx.mono().expect("mono"), Duration::from_nanos(7));
    assert_eq!(fx.wall_secs().expect("wall"), 1_760_000_000);
    let answer = fx
        .gh_for_tests(
            &transport,
            &Endpoints::github_com(),
            get_with("https://api.github.com/repos/o/r/releases/tags/v1", &[]),
        )
        .expect("replayed");
    assert_eq!(parts(answer).2, b"{\"tag_name\":\"v1\"}\n");
    fx.sleep(Duration::from_nanos(5)).expect("sleep");
    fx.emit_stdout(b"new output\n").expect("emit");
    fx.finish(0).expect("finish");
    assert!(transport.requests().is_empty());

    let (new_header, new_records) =
        tape::parse(&std::fs::read_to_string(&new).expect("the new tape")).expect("a tape");
    assert_eq!(new_header.agh_version, agh::VERSION);
    assert_eq!(new_header.argv, h.argv);
    assert_eq!(
        new_records,
        [
            EffectRecord::Mono(7),
            EffectRecord::Clock(1_760_000_000_000_000_000),
            exchange,
            EffectRecord::Exchange {
                door: DOOR_SLEEP.to_string(),
                request: b"5".to_vec(),
                reply: Vec::new(),
            },
            EffectRecord::Emit {
                door: DOOR_STDOUT.to_string(),
                bytes: b"new output\n".to_vec(),
            },
            EffectRecord::Emit {
                door: DOOR_EXIT.to_string(),
                bytes: b"0".to_vec(),
            },
        ]
    );
    assert_eq!(
        std::fs::read_to_string(&old).expect("the old tape"),
        old_text
    );
    let refused = Fx::rerecord_for_tests(&old, &new).expect_err("new exists");
    assert_eq!(
        (refused.kind, refused.sub.clone()),
        (Kind::CouldNotRun, Sub::Local)
    );
}

#[test]
fn a_rerecording_still_stops_at_a_divergence() {
    let dir = scratch("fx");
    let old = tape_path(dir.path(), "old");
    let new = tape_path(dir.path(), "new");
    std::fs::write(
        &old,
        tape::render(
            &header(&["x"]),
            &[EffectRecord::Mono(1), EffectRecord::Clock(2)],
        ),
    )
    .expect("the old tape");
    let (fx, _) = Fx::rerecord_for_tests(&old, &new).expect("a rerecording");
    fx.mono().expect("mono");
    let first = fx.finish(0).expect_err("a clock left");
    assert_eq!(shape(&first), replay_stop("unconsumed"));
    // A rerecording that diverged can never replay: the new tape is gone, so the live record
    // its next names is not refused for a file that exists.
    assert!(!new.exists());
    assert!(first.next.contains("--record") && !first.next.contains("--replay"));
}

#[test]
fn a_rerecording_asked_another_question_points_at_a_live_record() {
    let dir = scratch("fx");
    let old = tape_path(dir.path(), "old");
    let new = tape_path(dir.path(), "new");
    // Arguments a shell would split or read: each comes back as one word of the next.
    let argv = ["api", "/meta", "--jq", ".[] | .name", "-f", "q=it's"];
    let old_text = tape::render(&header(&argv), &[gh_exchange("/meta", b"{}")]);
    std::fs::write(&old, &old_text).expect("the old tape");
    let (fx, _) = Fx::rerecord_for_tests(&old, &new).expect("a rerecording");
    let transport = ScriptedTransport::new();
    let first = fx
        .gh_for_tests(
            &transport,
            &Endpoints::github_com(),
            get_with("https://api.github.com/zen", &[]),
        )
        .expect_err("another path");
    assert_eq!(shape(&first), replay_stop("request"));
    assert!(first.next.contains("--record") && !first.next.contains("--replay"));
    assert!(
        first
            .next
            .contains(r"api /meta --jq '.[] | .name' -f 'q=it'\''s'"),
        "{}",
        first.next
    );
    assert_eq!(&fx.finish(3).expect_err("latched"), &first);
    assert!(!new.exists());
    assert_eq!(
        std::fs::read_to_string(&old).expect("the old tape"),
        old_text
    );
    assert!(transport.requests().is_empty());
}
