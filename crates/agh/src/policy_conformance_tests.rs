// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Route-wall Tier-1 conformance tests for `Router::decide` and `hop::rebuild` (`AghRouteWall`).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;

use aterm_models_agh::agh_route_wall_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use aterm_spec::xref::{self, StepEvidence};

use super::*;
use crate::config::{self, Dirs, OwnerRule};
use crate::identity::{self, Credential, MapEnv};
use crate::wire::door::AuthHeader;
use crate::wire::hop;
use crate::wire::{Deadlines, Endpoints, Method, WireRequest};

const MACHINE: &str = "AghRouteWall";
/// The CDN location every hop is redirected to: one of `Endpoints::github_com()`'s hosts.
const CDN_URL: &str = "https://objects.githubusercontent.com/github-production-release-asset/1";

/// The eleven laws.
const LAWS: [&str; 11] = [
    "ReleaseTokenStaysHome",
    "TierWall",
    "NamedDestination",
    "NoAmbient",
    "NoAnonymousWriterOrWrite",
    "RefusalSendsNothing",
    "AnonymousCarriesNothing",
    "SendCarriesIdentity",
    "ExternalWriteOff",
    "HopDropsAuth",
    "NoWriteHop",
];

/// Each mutant and the law design §17.1 pairs it with.
const MUTANTS: [(&str, &str); 9] = [
    ("BuggyDecideFallback", "TierWall"),
    ("BuggyDecideAlias", "NamedDestination"),
    ("BuggyDecideReleaseAbroad", "ReleaseTokenStaysHome"),
    ("BuggyDecideAmbient", "NoAmbient"),
    ("BuggyAnonymousCarries", "AnonymousCarriesNothing"),
    ("BuggyDecideAnonWrite", "NoAnonymousWriterOrWrite"),
    ("BuggyRefuseButSend", "RefusalSendsNothing"),
    ("BuggyHopKeepsAuth", "HopDropsAuth"),
    ("BuggyHopOnWrite", "NoWriteHop"),
];

/// The seven world actions the waivers name.
const WORLD: [&str; 7] = [
    "NextTier",
    "NextOp",
    "ToggleNamed",
    "ToggleDev",
    "ToggleRel",
    "ToggleAmbient",
    "Reset",
];

fn var(state: &State, name: &str) -> i64 {
    state[name]
}

/// The model's `tier` as the canonical owners' repositories.
fn slug_text(tier: i64) -> &'static str {
    match tier {
        0 => "fixture-dev/repo",
        1 => "alabsystems/aterm",
        _ => "fixture-ext/repo",
    }
}

/// The model's `op` as an access class.
fn access(op: i64) -> Access {
    match op {
        0 => Access::Read,
        1 => Access::WriterRead,
        _ => Access::Write,
    }
}

/// A request for `state`'s tier, op and naming.
fn request(state: &State, named: Named) -> RouteRequest {
    let slug = Slug::parse(slug_text(var(state, "tier"))).expect("a canonical slug");
    let target = RouteTarget::Repo(slug);
    let req = match access(var(state, "op")) {
        Access::Read => RouteRequest::read(target),
        Access::WriterRead => RouteRequest::writer_read(target),
        Access::Write => RouteRequest::write(target),
    };
    req.with_named(named)
}

/// The namings `state` stands for: the literal for `named == 1`; `@channel` and the
/// library's default for `named == 0`, which must project alike.
fn namings(state: &State) -> Vec<Named> {
    if var(state, "named") == 1 {
        let typed = ExplicitTarget::from_argv(OsStr::new(slug_text(var(state, "tier"))))
            .expect("the CLI's literal");
        vec![Named::Literal(typed)]
    } else {
        vec![Named::Default, Named::Target("channel".to_string())]
    }
}

/// A scratch root holding the canonical agh.toml and the token files `state` has.
struct World {
    _root: aterm_tempfile::TempDir,
    router: Router,
}

/// The canonical config, written as a real agh.toml under `root` and read back by
/// `config::load`, with the dev token file iff `dev` and the release one iff `rel`.
/// A new setup file, 0600, written whole and NOT flushed: `write_private`, the store's
/// own writer, ends each write with two `F_FULLFSYNC`s, and a world per reachable state
/// made those flushes nearly all of this test's 4.1 s (0.26 s CPU). Nothing it reads
/// depends on the drive's cache.
fn setup_file(path: &std::path::Path, bytes: &[u8]) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut f| f.write_all(bytes))
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

fn world(dev: bool, rel: bool) -> World {
    let root = aterm_tempfile::Builder::new()
        .prefix("agh-wall-")
        .tempdir()
        .expect("a scratch root");
    let base = root.path();
    let text = super::tests::canonical_text(base);
    let dirs = Dirs::under(base);
    let parent = dirs.config.parent().expect("the config's parent");
    std::fs::create_dir_all(parent).expect("the config's parent");
    aterm_uds::private_file::create_private_dir(&dirs.config).expect("the config directory");
    setup_file(&dirs.config.join("agh.toml"), text.as_bytes());
    let token = |dir: &str, name: &str, seed: &str| {
        let dir = base.join(dir);
        aterm_uds::private_file::create_private_dir(&dir).expect("a token directory");
        let value = format!("{}\n", agh_fake::token(seed));
        setup_file(&dir.join(name), value.as_bytes());
    };
    if dev {
        token("tokens", "dev.token", "dev");
    }
    if rel {
        token("secrets", "release.token", "release");
    }
    let validated = config::load(&dirs)
        .expect("the canonical agh.toml loads")
        .expect("the canonical agh.toml is there");
    World {
        _root: root,
        router: Router::from_validated(validated),
    }
}

/// The environment `amb` stands for: `GH_TOKEN` set iff `amb`.
fn env(amb: bool) -> MapEnv {
    if amb {
        MapEnv::new().with("GH_TOKEN", &agh_fake::token("ambient"))
    } else {
        MapEnv::new()
    }
}

/// The real decision for `req` over `router`, presence judged by the real
/// `identity::presence` in `env`, inside a `Decide` window when `ev` is given.
fn decide(
    router: &Router,
    req: &RouteRequest,
    env: &MapEnv,
    ev: Option<&mut StepEvidence>,
) -> Decision {
    let presence = |name: &str| {
        router
            .profile(name)
            .map_or(Presence::Absent, |profile| identity::presence(profile, env))
    };
    match ev {
        Some(ev) => ev.step("Decide", || router.decide(req, &presence)),
        None => router.decide(req, &presence),
    }
}

/// `state` with `(phase, used)` put in, as the model's `Decide` writes them.
fn with_decision(state: &State, (phase, used): (i64, i64)) -> State {
    let mut next = state.clone();
    next.insert("phase", phase);
    next.insert("used", used);
    next
}

/// The one successor of `state` under `action`.
fn successor(model: &Model, state: &State, action: &str) -> Option<State> {
    let mut next = model.successors(action, state);
    assert!(next.len() <= 1, "{action} from {state:?}: {next:?}");
    next.pop()
}

/// A GET from the api origin of `endpoints`, carrying `auth` when given.
fn get<'a>(endpoints: &Endpoints, auth: Option<&'a AuthHeader>) -> WireRequest<'a> {
    let mut req = WireRequest::new(
        Method::Get,
        endpoints.url(
            crate::wire::OriginRole::Api,
            "/repos/alabsystems/aterm/releases/assets/1",
        ),
        vec![
            ("Accept".to_string(), "application/octet-stream".to_string()),
            (
                "X-GitHub-Api-Version".to_string(),
                crate::API_VERSION.to_string(),
            ),
        ],
        1 << 20,
        Deadlines::JSON,
    );
    req.auth = auth;
    req
}

/// What `Decide` reads of a state: tier, op, named, dev, rel, amb.
type Key = (i64, i64, i64, i64, i64, i64);

/// `state`'s key for the decision cache: everything `Decide` reads.
fn key(state: &State) -> Key {
    (
        var(state, "tier"),
        var(state, "op"),
        var(state, "named"),
        var(state, "dev"),
        var(state, "rel"),
        var(state, "amb"),
    )
}

#[test]
fn the_real_router_and_hop_conform_to_the_route_wall() {
    std::hint::black_box(crate::policy::route_wall_environment as fn());
    let model = agh_route_wall_model();
    let states = interp::reachable_states(&model);
    let mut ev = StepEvidence::new(MACHINE);

    let mut real: BTreeMap<Key, (i64, i64)> = BTreeMap::new();

    for state in states.iter().filter(|s| var(s, "phase") == 0) {
        let (dev, rel) = (var(state, "dev") == 1, var(state, "rel") == 1);
        let world = world(dev, rel);
        let amb = env(var(state, "amb") == 1);
        let expected = successor(&model, state, "Decide").expect("Decide fires in phase 0");
        let mut projected = BTreeSet::new();
        for named in namings(state) {
            let req = request(state, named);
            let decision = decide(&world.router, &req, &amb, Some(&mut ev));
            let projection = project_decision(&decision);
            assert_eq!(
                (expected["phase"], expected["used"]),
                projection,
                "{state:?}: {decision:?}"
            );
            let opposite = env(var(state, "amb") != 1);
            let other = decide(&world.router, &req, &opposite, Some(&mut ev));
            assert_eq!(
                other, decision,
                "GH_TOKEN changed the decision at {state:?}"
            );
            projected.insert(projection);
        }
        assert_eq!(
            projected.len(),
            1,
            "@channel and the default differ at {state:?}"
        );
        real.insert(key(state), projected.into_iter().next().expect("one"));
    }

    let endpoints = Endpoints::github_com();
    let cred = Credential::for_tests("dev", Tier::Dev, agh_fake::token("dev").as_bytes());
    let header = AuthHeader::for_origin(&cred, &endpoints, endpoints.api())
        .expect("a header for the api origin");
    for state in states.iter().filter(|s| var(s, "phase") > 0) {
        let auth = (var(state, "phase") == 1).then_some(&header);
        match successor(&model, state, "Hop") {
            Some(next) => {
                let req = get(&endpoints, auth);
                assert_eq!(hop::project_hop(&req).1, i64::from(auth.is_some()));
                let rebuilt = ev
                    .step("Hop", || {
                        hop::rebuild(&req, CDN_URL, &endpoints, "agh/test")
                    })
                    .expect("a read hops to the CDN");
                assert_eq!(hop::project_hop(&rebuilt), (1, 0));
                assert_eq!((next["hop"], next["hopauth"]), (1, 0));
            }
            None if var(state, "op") == 2 && var(state, "hop") == 0 => {
                let mut req = get(&endpoints, auth);
                req.method = Method::Patch;
                let refused = ev.step("Hop", || {
                    hop::rebuild(&req, CDN_URL, &endpoints, "agh/test")
                });
                let Err(error) = refused else {
                    panic!("a write hopped at {state:?}");
                };
                assert_eq!(error.kind, crate::error::Kind::Conflict);
            }
            None => {}
        }
    }

    let buggy = interp::with_buggy(&model, 1);
    for state in &states {
        for (mutant, law) in MUTANTS {
            for wrong in buggy.successors(mutant, state) {
                let lawful = if mutant.starts_with("BuggyHop") {
                    let mut next = state.clone();
                    if var(state, "op") <= 1 {
                        next.insert("hop", 1);
                        next.insert("hopauth", 0);
                    }
                    next
                } else {
                    with_decision(state, real[&key(state)])
                };
                assert_ne!(wrong, lawful, "{mutant} at {state:?}");
                assert!(
                    model.check_invariant(law, &lawful),
                    "{law} fails on the real step at {state:?}"
                );
                assert!(
                    !buggy.check_invariant(law, &wrong),
                    "{mutant} keeps {law} at {state:?}"
                );
            }
        }
    }

    for state in states
        .iter()
        .filter(|s| var(s, "phase") == 0 && var(s, "tier") == 1)
    {
        let world = world(var(state, "dev") == 1, var(state, "rel") == 1);
        let router = &forged_floor(&world);
        let amb = env(var(state, "amb") == 1);
        for named in namings(state) {
            let decision = decide(router, &request(state, named), &amb, Some(&mut ev));
            let projection = project_decision(&decision);
            let next = with_decision(state, projection);
            for law in LAWS {
                assert!(
                    model.check_invariant(law, &next),
                    "{law} at {state:?}: {decision:?}"
                );
            }
            if var(state, "op") > 0 {
                assert_eq!(projection, (3, 0), "{state:?}: {decision:?}");
            }
        }
    }

    let audit = ev.audit();
    assert!(audit.is_complete(), "{audit:?}");
    assert_eq!(
        audit.actions,
        ["Decide", "Hop"].into_iter().collect::<BTreeSet<_>>()
    );
    let bound: BTreeSet<&str> = xref::refinements()
        .filter(|r| r.machine == MACHINE)
        .map(|r| r.action)
        .collect();
    assert_eq!(bound, ["Decide", "Hop"].into_iter().collect());
    let waived: BTreeSet<&str> = xref::waivers()
        .filter(|w| w.machine == MACHINE)
        .map(|w| w.action)
        .collect();
    let expected: BTreeSet<&str> = WORLD
        .into_iter()
        .chain(MUTANTS.iter().map(|(mutant, _)| *mutant))
        .collect();
    assert_eq!(waived, expected);
    let every: BTreeSet<&str> = model.actions.iter().map(|a| a.name).collect();
    assert_eq!(
        bound.union(&waived).copied().collect::<BTreeSet<_>>(),
        every
    );
}

/// A router over `world`'s loaded `Validated` with the `alabsystems` row changed in the crate
/// to tier dev and a dev writer: a row L2 refuses in any file.
fn forged_floor(world: &World) -> Router {
    let mut validated = world
        .router
        .validated
        .clone()
        .expect("the world's router has a config");
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
