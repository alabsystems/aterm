// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The inline token source: `Profile::inline` and `Router::single_identity` (WP1).

use std::path::PathBuf;

use agh::error::{Kind, Sub};
use agh::identity::{self, InlineToken, LoadError, MapEnv, Profile, Source};
use agh::policy::{
    Decision, PermitTarget, Presence, RefusalCode, RouteRequest, RouteTarget, Router, Tier,
};
use aterm_uds::secret::Secret;

use crate::common;

fn inline(token: &str) -> InlineToken {
    InlineToken::new(Secret::new(token.as_bytes())).expect("an inline token")
}

fn fleet() -> Profile {
    Profile::inline(
        "fleet",
        Tier::Dev,
        "fixture-dev-bot",
        inline(&agh_fake::token("inline")),
    )
    .expect("a dev inline profile")
}

/// Each profile's presence on `router`, as the client judges it, with no environment.
fn presence_on(router: &Router) -> impl Fn(&str) -> Presence + '_ {
    |name: &str| {
        router.profile(name).map_or(Presence::Absent, |profile| {
            identity::presence(profile, &MapEnv::new())
        })
    }
}

fn code(decision: &Decision) -> Option<RefusalCode> {
    match decision {
        Decision::Refuse(refusal) => Some(refusal.code),
        _ => None,
    }
}

#[test]
fn an_inline_token_is_trimmed_and_named_by_its_fingerprint() {
    let t = agh_fake::token("inline");
    let bare = inline(&t);
    assert_eq!(inline(&format!("{t}\n")), bare);
    assert_eq!(inline(&format!("{t}\r\n")), bare);
    assert_eq!(bare.fingerprint(), identity::fingerprint(t.as_bytes()));
    let shown = format!("{bare:?}");
    assert_eq!(shown, format!("InlineToken({})", bare.fingerprint()));
    for run in t.as_bytes().windows(8) {
        let run = std::str::from_utf8(run).expect("ASCII");
        assert!(!shown.contains(run), "{shown} holds {run}");
    }
}

#[test]
fn an_inline_token_of_the_wrong_shape_is_refused() {
    let short = "x".repeat(19);
    let malformed = InlineToken::new(Secret::new(short.as_bytes())).expect_err("too short");
    assert!(
        matches!(malformed, LoadError::Malformed(_)),
        "{malformed:?}"
    );
    let refresh = format!("ghr_{}", "a".repeat(32));
    let unsupported =
        InlineToken::new(Secret::new(refresh.as_bytes())).expect_err("a refresh token");
    assert!(
        matches!(unsupported, LoadError::Unsupported(_)),
        "{unsupported:?}"
    );
}

#[test]
fn an_inline_profile_is_present_and_never_release() {
    let t = agh_fake::token("inline");
    let refused = Profile::inline("fleet", Tier::Release, "fixture-release-bot", inline(&t))
        .expect_err("an inline profile is never release tier");
    assert_eq!(refused.kind, Kind::Refused);
    assert_eq!(refused.exit(), 1);
    assert_eq!(refused.sub, Sub::Policy(RefusalCode::TierMismatch));
    let dev = fleet();
    assert!(matches!(dev.source, Source::Inline(_)));
    assert_eq!(dev.login.as_deref(), Some("fixture-dev-bot"));
    assert_eq!(identity::presence(&dev, &MapEnv::new()), Presence::Present);
    let cred = identity::load_for_tests(&dev, &MapEnv::new()).expect("a credential");
    assert_eq!(cred.fingerprint(), identity::fingerprint(t.as_bytes()));
    assert_eq!(cred.profile(), "fleet");
    assert_eq!(cred.tier(), Tier::Dev);
}

#[test]
fn single_identity_routes_the_identity_only() {
    let router = Router::single_identity(fleet());
    assert!(router.is_single_identity());
    let presence = presence_on(&router);
    for req in [
        RouteRequest::read(RouteTarget::Identity),
        RouteRequest::writer_read(RouteTarget::Identity),
        RouteRequest::write(RouteTarget::Identity),
    ] {
        assert_eq!(
            router.decide(&req, &presence),
            Decision::Send {
                profile: "fleet".into(),
                tier: Tier::Dev
            },
            "{req:?}"
        );
    }
    let repo = router.decide(
        &RouteRequest::read(RouteTarget::Repo(common::slug("fixture-dev/app"))),
        &presence,
    );
    assert_eq!(code(&repo), Some(RefusalCode::UnscopedNotRead));
    let permit = router
        .permit(&RouteRequest::write(RouteTarget::Identity), &presence)
        .expect("an identity write permits");
    assert_eq!(permit.target(), &PermitTarget::Identity);
    assert_eq!(permit.provenance(), None);
    assert!(permit.covers("/user/keys"));
    assert!(!permit.covers("/repos/fixture-dev/app"));
    assert!(router.writers().is_empty());
}

#[test]
fn single_identity_refuses_a_release_profile() {
    let profile = Profile::new(
        "release",
        Source::File(PathBuf::from("/nonexistent/release.token")),
        Tier::Release,
    );
    let router = Router::single_identity(profile);
    for req in [
        RouteRequest::read(RouteTarget::Identity),
        RouteRequest::write(RouteTarget::Identity),
        RouteRequest::read(RouteTarget::Repo(common::slug("alabsystems/aterm"))),
        RouteRequest::read(RouteTarget::Identity).with_as(Some("other".to_string())),
    ] {
        let decided = router.decide(&req, &presence_on(&router));
        assert_eq!(code(&decided), Some(RefusalCode::TierMismatch), "{req:?}");
    }
}
