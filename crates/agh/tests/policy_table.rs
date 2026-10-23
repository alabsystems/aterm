// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The Router over the canonical config (design §6.4).

use agh::error::{Kind, Sub, Tried};
use agh::identity::{self, MapEnv};
use agh::policy::{
    AnonymousWhy, Decision, Named, Presence, RefusalCode, RouteRequest, RouteTarget, Router, Tier,
};
use aterm_json::Value;

use crate::common;

/// The canonical router, over a fresh root.
fn canonical() -> Router {
    let root = common::scratch("table");
    common::router(&common::canonical_config(root.path()), root.path())
}

/// Presence: the dev profile `dev`, the release profile `rel`, every other profile absent.
fn pres(dev: Presence, rel: Presence) -> impl Fn(&str) -> Presence {
    move |name: &str| match name {
        "dev" => dev,
        "release" => rel,
        _ => Presence::Absent,
    }
}

const P: Presence = Presence::Present;
const A: Presence = Presence::Absent;

fn repo(text: &str) -> RouteTarget {
    RouteTarget::Repo(common::slug(text))
}

fn send(profile: &str, tier: Tier) -> Decision {
    Decision::Send {
        profile: profile.to_string(),
        tier,
    }
}

fn anonymous(why: AnonymousWhy) -> Decision {
    Decision::Anonymous { why }
}

/// The refusal's code, or `None`.
fn code(decision: &Decision) -> Option<RefusalCode> {
    match decision {
        Decision::Refuse(refusal) => Some(refusal.code),
        _ => None,
    }
}

const DEV: &str = "fixture-dev/repo";
const REL: &str = "alabsystems/aterm";
const EXT: &str = "fixture-ext/repo";

#[test]
fn the_decision_table_row_by_row() {
    let router = canonical();
    let read =
        |slug: &str, dev, rel| router.decide(&RouteRequest::read(repo(slug)), &pres(dev, rel));
    let writer_read = |slug: &str, dev, rel| {
        router.decide(&RouteRequest::writer_read(repo(slug)), &pres(dev, rel))
    };
    let write =
        |slug: &str, dev, rel| router.decide(&RouteRequest::write(repo(slug)), &pres(dev, rel));

    assert_eq!(read(DEV, P, P), send("dev", Tier::Dev));
    assert_eq!(code(&read(DEV, A, P)), Some(RefusalCode::ChainExhausted));
    assert_eq!(writer_read(DEV, P, P), send("dev", Tier::Dev));
    assert_eq!(
        code(&writer_read(DEV, A, P)),
        Some(RefusalCode::WriterAbsent)
    );
    assert_eq!(write(DEV, P, A), send("dev", Tier::Dev));
    assert_eq!(code(&write(DEV, A, P)), Some(RefusalCode::WriterAbsent));
    assert_eq!(RefusalCode::WriterAbsent.kind().exit(), 4);
    assert_eq!(RefusalCode::ChainExhausted.kind().exit(), 4);

    assert_eq!(read(REL, P, P), send("dev", Tier::Dev));
    assert_eq!(read(REL, A, P), anonymous(AnonymousWhy::ChainFellThrough));
    assert_eq!(writer_read(REL, P, P), send("release", Tier::Release));
    assert_eq!(
        code(&writer_read(REL, P, A)),
        Some(RefusalCode::WriterAbsent)
    );
    assert_eq!(
        code(&write(REL, P, P)),
        Some(RefusalCode::ReleaseNeedsLiteral)
    );
    assert_eq!(RefusalCode::ReleaseNeedsLiteral.kind().exit(), 1);

    assert_eq!(read(EXT, P, P), send("dev", Tier::Dev));
    assert_eq!(read(EXT, A, P), anonymous(AnonymousWhy::ChainFellThrough));
    assert_eq!(code(&writer_read(EXT, P, P)), Some(RefusalCode::WriteNone));
    assert_eq!(code(&write(EXT, P, P)), Some(RefusalCode::WriteNone));
    assert_eq!(RefusalCode::WriteNone.kind().exit(), 1);
}

#[test]
fn an_absent_step_falls_through_and_an_unusable_one_stops() {
    let router = canonical();
    let req = RouteRequest::read(repo(REL));
    assert_eq!(
        router.decide(&req, &pres(A, A)),
        anonymous(AnonymousWhy::ChainFellThrough)
    );
    let stopped = router.decide(&req, &pres(Presence::Unusable("loose"), P));
    let Decision::Refuse(refusal) = stopped else {
        panic!("an unusable step stops the chain: {stopped:?}");
    };
    assert_eq!(refusal.code, RefusalCode::ChainStoppedUnusable);
    assert_eq!(
        refusal.tried,
        [(
            agh::policy::ChainStep::Profile("dev".into()),
            Presence::Unusable("loose")
        )]
    );
    let error = refusal.into_error(Some(&common::slug(REL)));
    assert_eq!(error.kind, Kind::Unauthenticated);
    assert_eq!(error.exit(), 4);
    assert_eq!(
        error.tried,
        [Tried::Skipped {
            as_: "dev".into(),
            why: "loose".into()
        }]
    );
    assert_eq!(error.repo.as_deref(), Some(REL));
}

#[test]
fn as_picks_a_step_of_the_chain_or_the_writer() {
    let router = canonical();
    let read_as = |slug: &str, name: &str| {
        router.decide(
            &RouteRequest::read(repo(slug)).with_as(Some(name.into())),
            &pres(P, P),
        )
    };
    assert_eq!(read_as(REL, "dev"), send("dev", Tier::Dev));
    assert_eq!(
        code(&read_as(REL, "release")),
        Some(RefusalCode::AsNotInChain)
    );
    assert_eq!(
        code(&read_as(DEV, "ghost")),
        Some(RefusalCode::ProfileUnknown)
    );
    let writer_as = |name: &str| {
        router.decide(
            &RouteRequest::writer_read(repo(REL)).with_as(Some(name.into())),
            &pres(P, P),
        )
    };
    assert_eq!(writer_as("release"), send("release", Tier::Release));
    assert_eq!(code(&writer_as("dev")), Some(RefusalCode::AsNotWriter));
    let absent_as = router.decide(
        &RouteRequest::read(repo(REL)).with_as(Some("dev".into())),
        &pres(A, P),
    );
    assert_eq!(code(&absent_as), Some(RefusalCode::ChainExhausted));
}

#[test]
fn an_identity_path_speaks_as_the_named_profile() {
    let router = canonical();
    let identity = RouteRequest::read(RouteTarget::Identity);
    assert_eq!(
        code(&router.decide(&identity, &pres(P, P))),
        Some(RefusalCode::IdentityNeedsAs)
    );
    let as_dev = identity.clone().with_as(Some("dev".into()));
    assert_eq!(router.decide(&as_dev, &pres(P, P)), send("dev", Tier::Dev));
    assert_eq!(
        code(&router.decide(&as_dev, &pres(A, P))),
        Some(RefusalCode::ChainExhausted)
    );
    assert_eq!(
        code(&router.decide(&as_dev, &pres(Presence::Unusable("symlink"), P))),
        Some(RefusalCode::ChainStoppedUnusable)
    );
    let as_ghost = identity.with_as(Some("ghost".into()));
    assert_eq!(
        code(&router.decide(&as_ghost, &pres(P, P))),
        Some(RefusalCode::ProfileUnknown)
    );
}

#[test]
fn unscoped_paths_read_the_default_chain_only() {
    let router = canonical();
    assert_eq!(
        router.decide(&RouteRequest::read(RouteTarget::Unscoped), &pres(P, P)),
        send("dev", Tier::Dev)
    );
    assert_eq!(
        code(&router.decide(
            &RouteRequest::writer_read(RouteTarget::Unscoped),
            &pres(P, P)
        )),
        Some(RefusalCode::UnscopedNotRead)
    );
    assert_eq!(
        code(&router.decide(
            &RouteRequest::writer_read(RouteTarget::Identity),
            &pres(P, P)
        )),
        Some(RefusalCode::UnscopedNotRead)
    );
}

#[test]
fn anonymous_is_a_read_only() {
    let router = canonical();
    assert_eq!(
        router.decide(
            &RouteRequest::read(repo(DEV)).with_anonymous(true),
            &pres(P, P)
        ),
        anonymous(AnonymousWhy::Asked)
    );
    assert_eq!(
        code(&router.decide(
            &RouteRequest::writer_read(repo(DEV)).with_anonymous(true),
            &pres(P, P)
        )),
        Some(RefusalCode::AnonymousWriterRead)
    );
}

#[test]
fn no_config_reads_anonymously_and_writes_nothing() {
    let router = Router::empty();
    assert!(!router.has_config());
    assert_eq!(
        router.decide(&RouteRequest::read(repo(DEV)), &pres(P, P)),
        anonymous(AnonymousWhy::NoConfig)
    );
    assert_eq!(
        code(&router.decide(&RouteRequest::writer_read(repo(DEV)), &pres(P, P))),
        Some(RefusalCode::WriteNone)
    );
    assert_eq!(router.tier_of("alabsystems"), Tier::Release);
    assert_eq!(router.tier_of("fixture-dev"), Tier::External);
}

#[test]
fn a_floor_owner_with_no_row_is_release_tier_and_never_written() {
    let root = common::scratch("table-floor");
    let text = common::canonical_config(root.path()).replace(
        "[[owners]]\nname = \"alabsystems\"\ntier = \"release\"\nread = [\"dev\", \"anonymous\"]\nwrite = \"release\"\n",
        "",
    );
    assert!(!text.contains("name = \"alabsystems\""), "{text}");
    let router = common::router(&text, root.path());
    assert_eq!(router.tier_of("alabsystems"), Tier::Release);
    assert_eq!(
        router.read_chain(Some("alabsystems")),
        router.read_chain(None)
    );
    assert_eq!(
        router.decide(&RouteRequest::read(repo(REL)), &pres(A, P)),
        anonymous(AnonymousWhy::ChainFellThrough)
    );
    let refused = router.decide(&RouteRequest::writer_read(repo(REL)), &pres(P, P));
    assert_eq!(code(&refused), Some(RefusalCode::WriteNone));
}

/// The refusal for `req` with every profile `presence`, as an error.
fn refused(router: &Router, req: &RouteRequest, dev: Presence, rel: Presence) -> agh::Error {
    match router.decide(req, &pres(dev, rel)) {
        Decision::Refuse(refusal) => refusal.into_error(None),
        other => panic!("{req:?} was not refused: {other:?}"),
    }
}

/// The error's kind and sub-kind, and every one of `names` in its `why` or its `next`.
fn assert_refusal(error: &agh::Error, code: RefusalCode, kind: Kind, names: &[&str]) {
    assert_eq!(error.kind, kind, "{error:?}");
    assert_eq!(error.sub, Sub::Policy(code), "{error:?}");
    for name in names {
        assert!(
            error.why.contains(name) || error.next.contains(name),
            "{name} in {error:?}"
        );
    }
}

#[test]
fn every_refusal_a_valid_config_gives() {
    let router = canonical();
    let u = Presence::Unusable("malformed");
    let unauth = Kind::Unauthenticated;
    assert_refusal(
        &refused(&router, &RouteRequest::writer_read(repo(DEV)), A, P),
        RefusalCode::WriterAbsent,
        unauth,
        &[
            "fixture-dev",
            "agh auth add dev",
            "--owner fixture-dev",
            "--tier dev",
        ],
    );
    assert_refusal(
        &refused(&router, &RouteRequest::writer_read(repo(DEV)), u, P),
        RefusalCode::WriterUnusable,
        unauth,
        &["fixture-dev", "dev", "malformed"],
    );
    assert_refusal(
        &refused(&router, &RouteRequest::read(repo(DEV)), A, P),
        RefusalCode::ChainExhausted,
        unauth,
        &["fixture-dev"],
    );
    assert_refusal(
        &refused(&router, &RouteRequest::read(repo(DEV)), u, P),
        RefusalCode::ChainStoppedUnusable,
        unauth,
        &["dev", "fixture-dev", "malformed"],
    );
    assert_refusal(
        &refused(&router, &RouteRequest::writer_read(repo(EXT)), P, P),
        RefusalCode::WriteNone,
        Kind::Refused,
        &["fixture-ext"],
    );
    assert_refusal(
        &refused(&router, &RouteRequest::write(repo(REL)), P, P),
        RefusalCode::ReleaseNeedsLiteral,
        Kind::Refused,
        &["alabsystems", "-R alabsystems/NAME"],
    );
    assert_refusal(
        &refused(
            &router,
            &RouteRequest::write(repo(REL)).with_named(Named::Target("channel".into())),
            P,
            P,
        ),
        RefusalCode::ReleaseTargetForWrite,
        Kind::Refused,
        &["@channel", "alabsystems", "-R alabsystems/aterm"],
    );
    assert_refusal(
        &refused(
            &router,
            &RouteRequest::read(repo(REL)).with_as(Some("release".into())),
            P,
            P,
        ),
        RefusalCode::AsNotInChain,
        Kind::Refused,
        &[
            "--as release",
            "alabsystems",
            "agh whoami --for alabsystems/aterm",
        ],
    );
    assert_refusal(
        &refused(
            &router,
            &RouteRequest::read(RouteTarget::Owner("alabsystems".into()))
                .with_as(Some("release".into())),
            P,
            P,
        ),
        RefusalCode::AsNotInChain,
        Kind::Refused,
        &["--as release", "agh whoami --for alabsystems/NAME"],
    );
    assert_refusal(
        &refused(
            &router,
            &RouteRequest::writer_read(repo(REL)).with_as(Some("dev".into())),
            P,
            P,
        ),
        RefusalCode::AsNotWriter,
        Kind::Refused,
        &["--as dev", "alabsystems", "release"],
    );
    assert_refusal(
        &refused(
            &router,
            &RouteRequest::writer_read(repo(DEV)).with_anonymous(true),
            P,
            P,
        ),
        RefusalCode::AnonymousWriterRead,
        Kind::Usage,
        &["--anonymous"],
    );
    assert_refusal(
        &refused(
            &router,
            &RouteRequest::read(repo(DEV)).with_as(Some("ghost".into())),
            P,
            P,
        ),
        RefusalCode::ProfileUnknown,
        Kind::Usage,
        &["ghost"],
    );
    assert_refusal(
        &refused(&router, &RouteRequest::read(RouteTarget::Identity), P, P),
        RefusalCode::IdentityNeedsAs,
        Kind::Usage,
        &["--as"],
    );
    assert_refusal(
        &refused(&router, &RouteRequest::write(RouteTarget::Unscoped), P, P),
        RefusalCode::UnscopedNotRead,
        Kind::Usage,
        &[],
    );
}

/// The fourteenth: `config::load` of a file that breaks a rule (unix: a private agh.toml).
#[cfg(unix)]
#[test]
fn config_invalid_comes_from_load() {
    let root = common::scratch("table-invalid");
    let dirs = agh::config::Dirs::under(root.path());
    common::write_config(&dirs, "[owners]\n");
    let error = agh::config::load(&dirs).expect_err("an invalid agh.toml");
    assert_refusal(
        &error,
        RefusalCode::ConfigInvalid,
        Kind::Refused,
        &["agh policy check"],
    );
}

#[test]
fn tier_of_holds_the_floor() {
    let router = canonical();
    assert_eq!(router.tier_of("alabsystems"), Tier::Release);
    assert_eq!(router.tier_of("ALABSYSTEMS"), Tier::Release);
    assert_eq!(router.tier_of("fixture-dev"), Tier::Dev);
    assert_eq!(router.tier_of("Fixture-Dev"), Tier::Dev);
    assert_eq!(router.tier_of("fixture-ext"), Tier::External);
}

/// `explain` of `req` with `dev` and `rel`, as compact JSON.
fn explained(router: &Router, req: &RouteRequest, dev: Presence, rel: Presence) -> Value {
    router.explain(req, &pres(dev, rel)).to_json()
}

#[test]
fn explain_shows_the_walk() {
    let router = canonical();
    let sent = explained(&router, &RouteRequest::read(repo(DEV)), P, P);
    assert_eq!(
        sent.to_string(),
        r#"{"chain":[{"presence":"present","step":"dev"}],"decision":{"send":{"profile":"dev","tier":"dev"}},"op":"read","owner":"fixture-dev","tier":"dev","writer":{"presence":"present","profile":"dev"}}"#
    );
    let fell = explained(&router, &RouteRequest::read(repo(REL)), A, P);
    assert_eq!(
        fell.to_string(),
        r#"{"chain":[{"presence":"absent","step":"dev"},{"step":"anonymous"}],"decision":{"anonymous":{"why":"chain_fell_through"}},"op":"read","owner":"alabsystems","tier":"release","writer":{"presence":"present","profile":"release"}}"#
    );
    let absent_writer = explained(&router, &RouteRequest::writer_read(repo(REL)), P, A);
    assert_eq!(
        absent_writer["chain"].to_string(),
        r#"[{"presence":"present","step":"dev"},{"step":"anonymous"}]"#
    );
    assert_eq!(
        absent_writer["writer"].to_string(),
        r#"{"presence":"absent","profile":"release"}"#
    );
    assert_eq!(
        absent_writer["decision"]["refuse"]["code"],
        Value::from("writer_absent")
    );
    let refused = explained(&router, &RouteRequest::writer_read(repo(DEV)), A, P);
    assert_eq!(
        refused["chain"].to_string(),
        r#"[{"presence":"absent","step":"dev"}]"#
    );
    assert_eq!(refused["op"], Value::from("writer-read"));
    assert_eq!(refused["writer"]["profile"], Value::from("dev"));
    assert_eq!(refused["writer"]["presence"], Value::from("absent"));
    let refusal = refused["decision"]["refuse"]
        .as_object()
        .expect("a refusal object");
    let keys: Vec<&String> = refusal.keys().collect();
    assert_eq!(keys, ["code", "next", "why"]);
    assert_eq!(refusal["code"], Value::from("writer_absent"));
    let unusable = explained(
        &router,
        &RouteRequest::read(repo(DEV)),
        Presence::Unusable("loose"),
        P,
    );
    assert_eq!(
        unusable["chain"][0].to_string(),
        r#"{"presence":"unusable","step":"dev","why":"loose"}"#
    );
    assert_eq!(
        unusable["writer"].to_string(),
        r#"{"presence":"unusable","profile":"dev","why":"loose"}"#
    );
}

/// The real presence of `router`'s profiles in `env`.
fn real(router: &Router, env: &MapEnv) -> impl Fn(&str) -> Presence {
    move |name: &str| {
        router
            .profile(name)
            .map_or(Presence::Absent, |profile| identity::presence(profile, env))
    }
}

#[test]
fn an_ambient_token_changes_nothing_unless_a_profile_names_it() {
    let router = canonical();
    let quiet = MapEnv::new();
    let loud = MapEnv::new()
        .with("GH_TOKEN", &agh_fake::token("ambient"))
        .with("GITHUB_TOKEN", &agh_fake::token("ambient2"));
    for req in [
        RouteRequest::read(repo(DEV)),
        RouteRequest::read(repo(REL)),
        RouteRequest::read(repo(EXT)),
        RouteRequest::writer_read(repo(REL)),
        RouteRequest::read(RouteTarget::Unscoped),
    ] {
        assert_eq!(
            router.decide(&req, &real(&router, &quiet)),
            router.decide(&req, &real(&router, &loud)),
            "{req:?}"
        );
    }
    let root = common::scratch("table-env");
    let named = common::router(
        "[profiles.dev]\nenv = \"GH_TOKEN\"\ntier = \"dev\"\nlogin = \"fixture-dev-bot\"\n\n\
         [[owners]]\nname = \"fixture-dev\"\ntier = \"dev\"\nread = [\"dev\"]\nwrite = \"dev\"\n",
        root.path(),
    );
    let req = RouteRequest::read(repo(DEV));
    assert_eq!(
        named.decide(&req, &real(&named, &loud)),
        send("dev", Tier::Dev)
    );
    assert_eq!(
        code(&named.decide(&req, &real(&named, &quiet))),
        Some(RefusalCode::ChainExhausted)
    );
}
