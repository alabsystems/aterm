// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The transport's write guard (plan §4.6.4): a permit from `Router::permit` over an
//! in-memory router, a credential from `Credential::for_tests`, and `HttpTransport::direct`
//! against agh-fake's loopback server.

use std::ffi::OsStr;
use std::path::Path;
use std::sync::Arc;

use agh_fake::{FakeGithub, RepoSpec};

use super::HttpTransport;
use crate::config;
use crate::identity::{Credential, MapEnv, Profile, Source};
use crate::policy::{
    ExplicitTarget, Named, Permit, Presence, RouteRequest, RouteTarget, Router, Tier,
};
use crate::slug::Slug;
use crate::wire::door::AuthHeader;
use crate::wire::{
    Deadlines, Endpoints, Method, OriginRole, Phase, Transport, WireBody, WireError, WireKind,
    WireRequest,
};

const LIMIT: u64 = 1 << 20;

fn scratch() -> aterm_tempfile::TempDir {
    crate::test_fixtures::scratch_dir("permit")
}

/// The canonical config (`tests/fixtures/canonical.toml`) under `root`, as a router.
fn canonical(root: &Path) -> Router {
    let root_text = root
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let text = include_str!("../../tests/fixtures/canonical.toml").replace("{root}", &root_text);
    let parsed = config::parse(&text).expect("the canonical config parses");
    let validated =
        config::validate(&parsed, root, &config::file_id).expect("the canonical config validates");
    Router::from_validated(validated)
}

fn present(_: &str) -> Presence {
    Presence::Present
}

fn slug(text: &str) -> Slug {
    Slug::parse(text).expect("a slug")
}

/// A dev-tier permit for `fixture-dev/app`.
fn dev_permit(router: &Router) -> Permit {
    let req = RouteRequest::write(RouteTarget::Repo(slug("fixture-dev/app")));
    router.permit(&req, &present).expect("a dev-tier permit")
}

/// A release-tier permit for `alabsystems/aterm`, named by its literal.
fn release_permit(router: &Router) -> Permit {
    let literal = ExplicitTarget::from_argv(OsStr::new("alabsystems/aterm")).expect("a literal");
    let req = RouteRequest::write(RouteTarget::Repo(slug("alabsystems/aterm")))
        .with_named(Named::Literal(literal));
    router
        .permit(&req, &present)
        .expect("a release-tier permit")
}

fn endpoints(fake: &FakeGithub) -> Endpoints {
    let ports = fake.ports();
    Endpoints::loopback(ports.api, ports.uploads, ports.web, ports.cdn)
}

fn credential(profile: &str, tier: Tier) -> Credential {
    Credential::for_tests(profile, tier, agh_fake::token(profile).as_bytes())
}

fn auth(fake: &FakeGithub, cred: &Credential, role: OriginRole) -> AuthHeader {
    let endpoints = endpoints(fake);
    AuthHeader::for_origin(cred, &endpoints, endpoints.origin(role)).expect("a credential origin")
}

fn transport() -> HttpTransport {
    HttpTransport::direct(Arc::new(MapEnv::new()))
}

/// A POST of `path` on `role`'s origin, with `permit` and `auth`.
fn post<'a>(
    fake: &FakeGithub,
    role: OriginRole,
    path: &str,
    permit: &'a Permit,
    auth: Option<&'a AuthHeader>,
) -> WireRequest<'a> {
    let mut request = WireRequest::new(
        Method::Post,
        endpoints(fake).url(role, path),
        vec![("User-Agent".to_string(), "agh-test".to_string())],
        LIMIT,
        Deadlines::JSON,
    );
    request.permit = Some(permit);
    request.auth = auth;
    request
}

fn refused_unsent(fake: &FakeGithub, request: WireRequest<'_>) -> WireError {
    let error = transport()
        .round_trip(request)
        .expect_err("the guard refuses it");
    assert_eq!(
        (error.phase, error.kind),
        (Phase::NotSent, WireKind::Local),
        "{error:?}"
    );
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
    error
}

/// The request passed the guard: it reached the fake, whatever the fake answered.
fn sent(fake: &FakeGithub, request: WireRequest<'_>) {
    let before = fake.seen().len();
    if let Err(error) = transport().round_trip(request) {
        assert_ne!(
            (error.phase, error.kind),
            (Phase::NotSent, WireKind::Local),
            "{error:?}"
        );
    }
    assert_eq!(fake.seen().len(), before + 1, "{:?}", fake.seen());
}

fn fake() -> FakeGithub {
    let fake = FakeGithub::start();
    fake.add_repo("fixture-dev/app", RepoSpec::private());
    fake
}

#[test]
fn a_permit_for_another_repository_is_refused_unsent() {
    let root = scratch();
    let router = canonical(root.path());
    let fake = fake();
    let permit = release_permit(&router);
    let cred = credential("release", Tier::Release);
    let auth = auth(&fake, &cred, OriginRole::Api);
    let path = "/repos/fixture-dev/app/releases";
    let error = refused_unsent(
        &fake,
        post(&fake, OriginRole::Api, path, &permit, Some(&auth)),
    );
    assert_eq!(
        error.detail,
        "the permit covers alabsystems/aterm, not /repos/fixture-dev/app/releases"
    );
}

#[test]
fn an_identity_permit_reaches_only_user() {
    let root = scratch();
    let profile = Profile::new(
        "dev",
        Source::File(root.path().join("dev.token")),
        Tier::Dev,
    );
    let router = Router::single_identity(profile);
    let permit = router
        .permit(&RouteRequest::write(RouteTarget::Identity), &present)
        .expect("an identity permit");
    let fake = fake();
    let cred = credential("dev", Tier::Dev);
    let auth = auth(&fake, &cred, OriginRole::Api);
    let error = refused_unsent(
        &fake,
        post(&fake, OriginRole::Api, "/repos/x/y", &permit, Some(&auth)),
    );
    assert_eq!(error.detail, "the permit covers /user, not /repos/x/y");
    sent(
        &fake,
        post(&fake, OriginRole::Api, "/user/keys", &permit, Some(&auth)),
    );
}

#[test]
fn a_permit_of_another_profile_is_refused_unsent() {
    let root = scratch();
    let router = canonical(root.path());
    let fake = fake();
    let permit = dev_permit(&router);
    let cred = credential("release", Tier::Release);
    let auth = auth(&fake, &cred, OriginRole::Api);
    let path = "/repos/fixture-dev/app/releases";
    let error = refused_unsent(
        &fake,
        post(&fake, OriginRole::Api, path, &permit, Some(&auth)),
    );
    assert_eq!(
        error.detail,
        "the permit is dev's, the credential release's"
    );
    let error = refused_unsent(&fake, post(&fake, OriginRole::Api, path, &permit, None));
    assert_eq!(error.detail, "the permit is dev's, the credential none's");
}

#[test]
fn a_permit_of_a_profile_named_none_needs_its_credential() {
    let root = scratch();
    let profile = Profile::new(
        "none",
        Source::File(root.path().join("none.token")),
        Tier::Dev,
    );
    let router = Router::single_identity(profile);
    let permit = router
        .permit(&RouteRequest::write(RouteTarget::Identity), &present)
        .expect("an identity permit");
    let fake = fake();
    let error = refused_unsent(
        &fake,
        post(&fake, OriginRole::Api, "/user/keys", &permit, None),
    );
    assert_eq!(error.detail, "the permit is none's, the credential none's");
}

#[test]
fn a_repo_permit_reaches_the_api_and_uploads_origins_only() {
    let root = scratch();
    let router = canonical(root.path());
    let fake = fake();
    let permit = dev_permit(&router);
    let cred = credential("dev", Tier::Dev);
    let api = auth(&fake, &cred, OriginRole::Api);
    let path = "/repos/fixture-dev/app/releases";
    let error = refused_unsent(
        &fake,
        post(&fake, OriginRole::Web, path, &permit, Some(&api)),
    );
    let web = endpoints(&fake).origin(OriginRole::Web).base();
    assert_eq!(
        error.detail,
        format!("the permit covers fixture-dev/app, not {web}{path}")
    );
    sent(
        &fake,
        post(&fake, OriginRole::Api, path, &permit, Some(&api)),
    );
    let uploads = auth(&fake, &cred, OriginRole::Uploads);
    sent(
        &fake,
        post(
            &fake,
            OriginRole::Uploads,
            "/repos/fixture-dev/app/releases/1/assets?name=a.txt",
            &permit,
            Some(&uploads),
        ),
    );
}

/// Plan §7 places this case in `tests/wire_loopback.rs`; a permit can be set on a
/// `WireRequest` only inside the crate (`permit` is `pub(crate)`), so it lives here.
#[test]
fn a_permitted_write_streams_a_file_body() {
    let root = scratch();
    let router = canonical(root.path());
    let fake = fake();
    let permit = dev_permit(&router);
    let cred = credential("dev", Tier::Dev);
    let uploads = auth(&fake, &cred, OriginRole::Uploads);
    let bytes: Vec<u8> = (0..=250u8).cycle().take(100_000).collect();
    let len = u64::try_from(bytes.len()).expect("a length");
    let mut reader: &[u8] = &bytes;
    let mut request = post(
        &fake,
        OriginRole::Uploads,
        "/repos/fixture-dev/app/releases/1/assets?name=a.bin",
        &permit,
        Some(&uploads),
    );
    request.headers.push((
        "Content-Type".to_string(),
        "application/octet-stream".to_string(),
    ));
    request.body = WireBody::Reader {
        len,
        reader: &mut reader,
    };
    sent(&fake, request);
    let seen = fake.seen();
    let length = bytes.len().to_string();
    assert_eq!(
        seen.last().and_then(|s| s.header("content-length")),
        Some(length.as_str())
    );
    let mut hasher = aterm_digest::Sha256::new();
    hasher.update(&bytes);
    let digest = format!(
        "sha256:{}",
        aterm_codec::hex::encode_lower(&hasher.finalize())
    );
    let bodies = fake.bodies();
    assert_eq!(bodies.len(), 1, "{bodies:?}");
    assert_eq!(
        (bodies[0].len, bodies[0].sha256.as_str()),
        (len, digest.as_str())
    );
}
