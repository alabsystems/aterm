// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! policy's in-crate tests: the decide table over a `Validated` the crate forges, and
//! the `Named::Literal` cases only agh can make.

use std::cell::RefCell;
use std::ffi::OsStr;
use std::path::Path;

use super::*;
use crate::config::{self, OwnerRule};
use crate::error::Kind;

/// The canonical config (`tests/fixtures/canonical.toml`) with `{root}` filled, escaped for a
/// TOML basic string as `common::canonical_config` escapes it.
pub(super) fn canonical_text(root: &Path) -> String {
    let root = root
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    include_str!("../tests/fixtures/canonical.toml").replace("{root}", &root)
}

/// The canonical config, parsed and validated over `root`.
fn canonical(root: &Path) -> Validated {
    let parsed = config::parse(&canonical_text(root)).expect("the canonical config parses");
    config::validate(&parsed, root, &config::file_id).expect("the canonical config validates")
}

/// A scratch root, absolute on every platform.
fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("agh-policy-")
        .tempdir()
        .expect("a scratch directory")
}

/// Every profile Present.
fn present(_: &str) -> Presence {
    Presence::Present
}

fn repo(text: &str) -> Slug {
    Slug::parse(text).expect("a slug")
}

fn literal(text: &str) -> Named {
    Named::Literal(ExplicitTarget::from_argv(OsStr::new(text)).expect("a literal"))
}

/// The refusal's code, or `None` for any other decision.
fn code(decision: &Decision) -> Option<RefusalCode> {
    match decision {
        Decision::Refuse(refusal) => Some(refusal.code),
        _ => None,
    }
}

/// The canonical config with the `alabsystems` row moved to tier dev and a dev writer: a
/// row L2 refuses in any file.
fn forged_floor(root: &Path) -> Router {
    let mut validated = canonical(root);
    validated.owners.insert(
        "alabsystems".to_string(),
        OwnerRule {
            name: "alabsystems".to_string(),
            tier: Tier::Dev,
            read: vec![ChainStep::Profile("dev".into()), ChainStep::Anonymous],
            write: Some("dev".to_string()),
        },
    );
    Router::from_validated(validated)
}

#[test]
fn a_literal_release_write_sends_as_release() {
    let root = scratch();
    let router = Router::from_validated(canonical(root.path()));
    let target = || RouteTarget::Repo(repo("alabsystems/aterm"));
    let sent = router.decide(
        &RouteRequest::write(target()).with_named(literal("alabsystems/aterm")),
        &present,
    );
    assert_eq!(
        sent,
        Decision::Send {
            profile: "release".into(),
            tier: Tier::Release
        }
    );
    // The literal is compared ASCII case-insensitively with the request's repository.
    let cased = router.decide(
        &RouteRequest::write(target()).with_named(literal("ALABSYSTEMS/Aterm")),
        &present,
    );
    assert_eq!(
        cased,
        Decision::Send {
            profile: "release".into(),
            tier: Tier::Release
        }
    );
    let other = router.decide(
        &RouteRequest::write(target()).with_named(literal("alabsystems/other")),
        &present,
    );
    assert_eq!(code(&other), Some(RefusalCode::ReleaseNeedsLiteral));
}

#[test]
fn a_target_is_no_literal() {
    let error = ExplicitTarget::from_argv(OsStr::new("@channel")).expect_err("a target");
    assert_eq!(error.kind, Kind::Usage);
    assert!(error.why.contains("@channel"), "{}", error.why);
}

#[test]
fn the_forged_floor_holds_at_decide_time() {
    let root = scratch();
    let router = forged_floor(root.path());
    assert_eq!(router.tier_of("alabsystems"), Tier::Release);
    let target = || RouteTarget::Repo(repo("alabsystems/aterm"));
    let writer_read = router.decide(&RouteRequest::writer_read(target()), &present);
    assert_eq!(code(&writer_read), Some(RefusalCode::TierMismatch));
    let write = router.decide(
        &RouteRequest::write(target()).with_named(literal("alabsystems/aterm")),
        &present,
    );
    assert_eq!(code(&write), Some(RefusalCode::TierMismatch));
    // A Read sends dev, as the canonical config's chain does.
    let read = router.decide(&RouteRequest::read(target()), &present);
    assert_eq!(
        read,
        Decision::Send {
            profile: "dev".into(),
            tier: Tier::Dev
        }
    );
}

#[test]
fn the_forged_chain_is_refused_before_presence() {
    let root = scratch();
    let mut validated = canonical(root.path());
    if let Some(row) = validated.owners.get_mut("fixture-dev") {
        row.read = vec![ChainStep::Profile("release".into()), ChainStep::Anonymous];
    }
    let router = Router::from_validated(validated);
    let asked = RefCell::new(Vec::<String>::new());
    let recording = |name: &str| {
        asked.borrow_mut().push(name.to_string());
        Presence::Present
    };
    let target = || RouteTarget::Repo(repo("fixture-dev/repo"));
    let read = router.decide(&RouteRequest::read(target()), &recording);
    assert_eq!(code(&read), Some(RefusalCode::TierMismatch));
    let as_release = router.decide(
        &RouteRequest::read(target()).with_as(Some("release".into())),
        &recording,
    );
    assert_eq!(code(&as_release), Some(RefusalCode::TierMismatch));
    assert_eq!(asked.into_inner(), Vec::<String>::new());
}

#[test]
fn tier_mismatch_and_method_not_read_are_refusals() {
    for (code, slug) in [
        (RefusalCode::TierMismatch, "tier_mismatch"),
        (RefusalCode::MethodNotRead, "method_not_read"),
    ] {
        assert_eq!(code.kind(), Kind::Refused);
        assert_eq!(code.kind().exit(), 1);
        assert_eq!(code.slug(), slug);
        let error = Refusal {
            code,
            why: "x".into(),
            next: "y".into(),
            tried: Vec::new(),
        }
        .into_error(Some(&repo("fixture-dev/repo")));
        assert_eq!(error.kind, Kind::Refused);
        assert_eq!(error.sub, crate::error::Sub::Policy(code));
        assert_eq!(error.repo.as_deref(), Some("fixture-dev/repo"));
    }
}
