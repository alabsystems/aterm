// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn prepared(body: impl FnOnce() -> Result<OneShotPost>) -> Vec<String> {
    let _github = crate::github::Fixture {
        origins: aterm_update_core::github_rest::Origins::loopback(1, 2, 3, 4),
        token: Some("ghs_fixtureOneShot0123".to_string()),
        spawn: None,
    }
    .enter();
    body().expect("prepared").argv()
}

#[test]
fn an_asset_upload_streams_off_disk_and_never_buffers() {
    let dir = crate::test_scratch_dir("oneshot");
    let dmg = dir.path().join("aterm-0.33.0.dmg");
    fs::write(&dmg, b"dmg").expect("the asset");
    let args = prepared(|| {
        OneShotPost::prepare_binary("repos/o/r/releases/1/assets?name=aterm-0.33.0.dmg", &dmg)
    });
    assert!(
        args.iter().any(|a| a == "--upload-file"),
        "the asset leg must stream: {args:?}"
    );
    assert!(
        !args.iter().any(|a| a == "--data-binary"),
        "a gigabyte payload must never be buffered: {args:?}"
    );
    let request = args.iter().position(|a| a == "-X").unwrap();
    assert_eq!(args[request + 1], "POST");
    let flag = args.iter().position(|a| a == "--upload-file").unwrap();
    assert_eq!(args[flag + 1], dmg.to_str().unwrap());
    assert_eq!(
        args.last().map(String::as_str),
        Some("http://127.0.0.1:2/repos/o/r/releases/1/assets?name=aterm-0.33.0.dmg"),
        "the uploads origin"
    );
    let args = prepared(|| OneShotPost::prepare_json("repos/o/r/releases", b"{}"));
    let flag = args.iter().position(|a| a == "--data-binary").unwrap();
    assert!(args[flag + 1].starts_with('@'), "{args:?}");
    assert!(!args.iter().any(|a| a == "--upload-file"));
    assert!(args.iter().any(|a| a == "--fail-with-body"), "{args:?}");
    assert!(args.windows(2).any(|w| w == ["-H", "@-"]), "{args:?}");
    assert!(
        !args.iter().any(|a| a.contains("ghs_fixtureOneShot0123")),
        "{args:?}"
    );
    assert_eq!(args.first().map(String::as_str), Some("/usr/bin/curl"));
}

fn out(status: i32, stderr: &str) -> RunOut {
    RunOut {
        status,
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

#[test]
fn only_a_local_curl_refusal_counts_as_never_sent() {
    assert!(
        transport_never_started(&out(2, "curl: option --data-binary: out of memory")),
        "curl exit 2 is argument/init failure, strictly before connect"
    );
    for (status, why) in [
        (0, "success"),
        (22, "HTTP error returned"),
        (28, "timeout — the request may have been delivered"),
        (56, "recv failure — likewise"),
        (7, "connect failed, but after the attempt began"),
        (-1, "killed; unknowable"),
    ] {
        assert!(
            !transport_never_started(&out(status, why)),
            "exit {status} ({why}) must NOT license a retry"
        );
    }
}

#[test]
fn a_four_hundred_is_a_refusal_and_everything_else_is_unknown() {
    let out = |status: i32, stderr: &str| RunOut {
        status,
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    };
    let refused = out(22, "curl: (22) The requested URL returned error: 422\n");
    assert!(
        server_refused_without_creating(&refused),
        "an answered 422 created nothing"
    );
    assert!(
        server_refused_without_creating(&out(
            22,
            "curl: (22) The requested URL returned error: 404\n"
        )),
        "so did an answered 404"
    );
    for unknown in [
        out(22, "curl: (22) The requested URL returned error: 500\n"),
        out(22, "curl: (22) The requested URL returned error: 502\n"),
        out(28, "curl: (28) Operation timed out\n"),
        out(56, "curl: (56) Recv failure: Connection reset by peer\n"),
        out(
            22,
            "curl: (22) some future wording this code has never seen\n",
        ),
        out(2, "curl: (2) failed to initialise\n"),
    ] {
        assert!(
            !server_refused_without_creating(&unknown),
            "{:?} must read as UNKNOWN, not as a refusal",
            unknown.stderr_utf8().trim()
        );
    }
    assert_eq!(
        http_status_from_curl_failure("curl: (22) The requested URL returned error: 422\n"),
        Some(422)
    );
    assert_eq!(http_status_from_curl_failure("no status here"), None);
}

#[test]
fn a_retracted_intent_makes_the_next_attempt_postable_again() {
    assert_eq!(
        durable_post_decision(true, false),
        DurablePostDecision::AwaitVisibility,
        "the wedge this fixes"
    );
    assert_eq!(
        durable_post_decision(false, false),
        DurablePostDecision::PersistIntentThenPost,
        "after retraction the asset is uploadable again"
    );
    assert_eq!(
        durable_post_decision(false, true),
        DurablePostDecision::ConvergeVisible,
        "and a visible object still converges rather than reposting"
    );
}
