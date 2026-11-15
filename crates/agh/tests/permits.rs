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
