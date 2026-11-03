// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Write permits: `Router::permit`, the guard on each request and its target rules (WP3).

use std::sync::Arc;

use agh::wire::http::HttpTransport;
use agh::wire::{Deadlines, Method, OriginRole, Phase, Transport, WireKind, WireRequest};
use agh_fake::{FakeGithub, RepoSpec};

use crate::common;

const LIMIT: u64 = 1 << 20;

fn bare<'a>(fake: &FakeGithub, method: Method, path: &str) -> WireRequest<'a> {
    WireRequest::new(
        method,
        common::endpoints(fake).url(OriginRole::Api, path),
        vec![("User-Agent".to_string(), "agh-test".to_string())],
        LIMIT,
        Deadlines::JSON,
    )
}

#[test]
fn transport_refuses_write_without_permit() {
    let fake = FakeGithub::start();
    fake.add_repo("fixture-dev/app", RepoSpec::public());
    let transport = HttpTransport::direct(Arc::new(common::env()));
    for method in [Method::Post, Method::Patch, Method::Put, Method::Delete] {
        let error = transport
            .round_trip(bare(&fake, method, "/repos/fixture-dev/app/releases"))
            .expect_err("a write with no permit is refused");
        assert_eq!(
            (error.phase, error.kind),
            (Phase::NotSent, WireKind::Local),
            "{error:?}"
        );
        assert!(error.detail.contains(method.as_str()), "{error:?}");
    }
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
}

#[test]
fn a_read_needs_no_permit() {
    let fake = FakeGithub::start();
    fake.add_repo("fixture-dev/app", RepoSpec::public());
    let transport = HttpTransport::direct(Arc::new(common::env()));
    let response = transport
        .round_trip(bare(&fake, Method::Get, "/repos/fixture-dev/app"))
        .expect("a read is sent");
    assert_eq!(response.status, 200);
    assert_eq!(fake.seen().len(), 1);
}

#[cfg(unix)]
#[test]
fn the_scripted_transport_records_each_permit() {
    use agh::Call;
    use agh::github::raw::RawSend;
    use agh::policy::{RouteRequest, RouteTarget};
    use agh::wire::scripted::ScriptedTransport;

    let root = common::scratch("scripted-permits");
    common::write_token(
        &root.path().join("tokens"),
        "dev.token",
        &agh_fake::token("dev"),
    );
    let router = common::router(&common::canonical_config(root.path()), root.path());
    let transport = Arc::new(ScriptedTransport::new());
    let gh = common::github_over(router, transport.clone(), common::scripted_endpoints(9000));
    let slug = common::slug("fixture-dev/app");
    transport.answer(200, &[("content-type", "application/json")], b"{}");
    transport.answer(200, &[("content-type", "application/json")], b"{\"id\":7}");
    gh.call(&Call::get(
        "/repos/fixture-dev/app",
        RouteRequest::read(RouteTarget::Repo(slug.clone())),
    ))
    .expect("the read");
    let send = RawSend::new(
        Method::Patch,
        "/repos/fixture-dev/app",
        RouteRequest::write(RouteTarget::Repo(slug.clone())),
    );
    let permit = gh.permit(&send.route).expect("a dev-tier permit");
    let dirs = agh::config::Dirs::under(root.path());
    let journal = agh::Journal::open(&dirs, &slug, gh.fx()).expect("the journal");
    let outcome = gh.raw().send(&permit, &send, &journal).expect("the PATCH");
    assert!(outcome.done().is_some(), "the PATCH is done");
    assert_eq!(transport.permits(), vec![None, Some("dev".to_string())]);
}

/// Off unix a write is never sent (`write::unix_only`), so `agh api`'s dry run answers what
/// the real run answers: the same exit and the same `not_sent` report but for its `dry_run`
/// mark, and nothing reaches the fake.
/// The writer is an env profile, which loads off unix as a token file does not.
#[cfg(not(unix))]
#[test]
fn off_unix_an_api_dry_run_answers_as_the_real_run() {
    let root = common::scratch("api-off-unix");
    let fake = FakeGithub::start();
    fake.add_repo(
        "fixture-dev/app",
        RepoSpec::private().push("fixture-dev-bot"),
    );
    let token = agh_fake::token("dev");
    fake.add_token(&token, "fixture-dev-bot");
    let dirs = agh::config::Dirs::under(root.path());
    std::fs::create_dir_all(&dirs.config).expect("the config directory");
    std::fs::write(
        dirs.config.join("agh.toml"),
        "[profiles.dev]\nenv = \"AGH_FIXTURE_TOKEN\"\ntier = \"dev\"\nlogin = \"fixture-dev-bot\"\n\n\
         [[owners]]\nname = \"fixture-dev\"\ntier = \"dev\"\nread = [\"dev\"]\nwrite = \"dev\"\n",
    )
    .expect("agh.toml");
    let run = |dry_run: bool| {
        let env = common::env().with("AGH_FIXTURE_TOKEN", &token);
        let (mut ctx, captured) = common::ctx(root.path(), &fake, env);
        let mut args = vec![
            "api",
            "-X",
            "POST",
            "/repos/fixture-dev/app/git/refs",
            "-f",
            "ref=refs/tags/v1",
        ];
        if dry_run {
            args.push("--dry-run");
        }
        let exit = common::run_cli(&mut ctx, &args);
        let report: aterm_json::Value =
            aterm_json::from_str(captured.stdout_text().trim_end()).expect("the report");
        (exit, report)
    };
    let (dry_exit, mut dry) = run(true);
    let (real_exit, mut real) = run(false);
    assert_eq!(
        dry_exit, real_exit,
        "the dry run predicts the real run's exit"
    );
    assert_eq!(dry.get("dry_run"), Some(&aterm_json::Value::from(true)));
    assert_eq!(real.get("dry_run"), Some(&aterm_json::Value::from(false)));
    dry.as_object_mut().expect("an object").remove("dry_run");
    real.as_object_mut().expect("an object").remove("dry_run");
    assert_eq!(dry, real, "the dry run predicts the real run's report");
    assert_eq!(
        real.get("outcome"),
        Some(&aterm_json::Value::from("not_sent"))
    );
    assert!(
        fake.seen().iter().all(|seen| seen.method == "GET"),
        "{:?}",
        fake.seen()
    );
}
