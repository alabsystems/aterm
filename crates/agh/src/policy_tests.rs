// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! In-crate policy tests for `Router::decide` and `Named::Literal` cases.

use std::cell::RefCell;
use std::ffi::OsStr;
use std::path::Path;

use super::*;
use crate::config::{self, OwnerRule};
use crate::error::Kind;
use crate::test_fixtures::canonical_text;

fn canonical(root: &Path) -> Validated {
    let parsed = config::parse(&canonical_text(root)).expect("the canonical config parses");
    config::validate(&parsed, root, &config::file_id).expect("the canonical config validates")
}

fn scratch() -> aterm_tempfile::TempDir {
    crate::test_fixtures::scratch_dir("policy")
}

fn present(_: &str) -> Presence {
    Presence::Present
}

fn repo(text: &str) -> Slug {
    Slug::parse(text).expect("a slug")
}

fn literal(text: &str) -> Named {
    Named::Literal(ExplicitTarget::from_argv(OsStr::new(text)).expect("a literal"))
}

fn code(decision: &Decision) -> Option<RefusalCode> {
    match decision {
        Decision::Refuse(refusal) => Some(refusal.code),
        _ => None,
    }
}

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

/// M1's identity rule, kept outside the model (whose `ident` binds `--as` with dev): an
/// Identity Read with `--as release` sends the release token when it is present.
#[test]
fn release_as_on_the_identity_sends_the_release_token() {
    let root = scratch();
    let router = Router::from_validated(canonical(root.path()));
    let sent = router.decide(
        &RouteRequest::read(RouteTarget::Identity).with_as(Some("release".into())),
        &present,
    );
    assert_eq!(
        sent,
        Decision::Send {
            profile: "release".into(),
            tier: Tier::Release
        }
    );
}

#[test]
fn permit_decided_takes_the_decision_it_is_given() {
    let root = scratch();
    let router = Router::from_validated(canonical(root.path()));
    let write = RouteRequest::write(RouteTarget::Repo(repo("alabsystems/aterm")))
        .with_named(literal("alabsystems/aterm"));
    let send = || Decision::Send {
        profile: "release".into(),
        tier: Tier::Release,
    };
    for router in [&router, &Router::empty()] {
        let permit = router
            .permit_decided(&write, send())
            .expect("a Send mints a permit");
        assert_eq!(
            permit.target(),
            &PermitTarget::Repo(repo("alabsystems/aterm"))
        );
        assert_eq!(permit.profile(), "release");
        assert_eq!(permit.tier(), Tier::Release);
        assert_eq!(permit.provenance(), Some(Provenance::Argv));
        assert_eq!(permit.grant(), None);
        assert_eq!(permit.named(), literal("alabsystems/aterm"));
    }
    let anonymous = router
        .permit_decided(
            &write,
            Decision::Anonymous {
                why: AnonymousWhy::Asked,
            },
        )
        .expect_err("a write is never anonymous");
    assert_eq!(anonymous.kind, Kind::Refused);
    assert_eq!(
        anonymous.sub,
        crate::error::Sub::Policy(RefusalCode::WriteNone)
    );
    let refusal = Refusal {
        code: RefusalCode::WriterAbsent,
        why: "absent".into(),
        next: "add it".into(),
        tried: Vec::new(),
    };
    let expected = refusal.clone().into_error(Some(&repo("alabsystems/aterm")));
    let refused = router
        .permit_decided(&write, Decision::Refuse(refusal))
        .expect_err("a refusal mints nothing");
    assert_eq!(refused.kind, expected.kind);
    assert_eq!(refused.sub, expected.sub);
}

/// A grant made from a scratch agh.toml that `config::load` read decides exactly as the argv
/// literal of the same slug, for every access, with the release token present and absent.
#[cfg(unix)]
#[test]
fn a_granted_literal_decides_as_an_argv_literal() {
    let root = scratch();
    let dirs = config::Dirs::under(root.path());
    let text = format!(
        "{}\n[[writers]]\ncaller = \"fixture-caller\"\nrepos = [\"alabsystems/aterm\"]\n",
        canonical_text(root.path())
    );
    crate::test_fixtures::write_config(&dirs, &text);
    let router = Router::from_validated(
        config::load(&dirs)
            .expect("agh.toml loads")
            .expect("agh.toml is there"),
    );
    let slug = repo("alabsystems/aterm");
    let granted = router
        .grant("fixture-caller", &slug)
        .expect("the row grants it");
    assert_eq!(granted.provenance(), Provenance::Granted);
    for release in [Presence::Present, Presence::Absent] {
        let presence = |name: &str| {
            if name == "release" {
                release
            } else {
                Presence::Present
            }
        };
        for target in [
            RouteRequest::read(RouteTarget::Repo(slug.clone())),
            RouteRequest::writer_read(RouteTarget::Repo(slug.clone())),
            RouteRequest::write(RouteTarget::Repo(slug.clone())),
        ] {
            let by_grant = router.decide(
                &target.clone().with_named(Named::Literal(granted.clone())),
                &presence,
            );
            let by_argv = router.decide(
                &target.clone().with_named(literal("alabsystems/aterm")),
                &presence,
            );
            assert_eq!(by_grant, by_argv, "{target:?} with release {release:?}");
        }
    }
}

#[test]
fn every_unusable_presence_round_trips_its_slug() {
    for kind in super::LOAD_KINDS {
        let presence = kind.presence();
        assert_eq!(
            Presence::from_slug(presence.slug()),
            Some(presence),
            "{kind:?}"
        );
    }
    assert_eq!(Presence::from_slug("present"), Some(Presence::Present));
    for junk in ["", "taped", "missing", "Present", "loose_mode"] {
        assert_eq!(Presence::from_slug(junk), None, "{junk:?}");
    }
}
