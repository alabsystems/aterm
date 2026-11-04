// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Route-wall Tier-1 conformance tests for `Router::decide` and `hop::rebuild` (`AghRouteWall`),
//! over the canonical agh.toml with its `[[writers]]` grant (`Vault = 0`) and over the rows vault
//! adoption derives with no agh.toml (`Vault = 1`, design §6.10).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;

use aterm_models_agh::agh_route_wall_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use aterm_spec::xref::{self, StepEvidence};

use super::*;
use crate::config::{self, Dirs, OwnerRule, Validated};
use crate::identity::vault::{self, Slot};
use crate::identity::{self, Credential, MapEnv};
use crate::test_fixtures::{setup_file, write_config};
use crate::wire::door::AuthHeader;
use crate::wire::hop;
use crate::wire::{Deadlines, Endpoints, Method, WireRequest};

const MACHINE: &str = "AghRouteWall";
const CDN_URL: &str = "https://objects.githubusercontent.com/github-production-release-asset/1";

/// `tests/common.rs`'s `WRITERS`, which the crate's own tests cannot reach: the grant the
/// worlds' agh.toml holds.
const WRITERS: &str =
    "\n[[writers]]\ncaller = \"fixture-caller\"\nrepos = [\"alabsystems/aterm\"]\n";
/// The caller `WRITERS` grants.
const CALLER: &str = "fixture-caller";

const LAWS: [&str; 12] = [
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
    "IdentityNamed",
];

const MUTANTS: [(&str, &str); 11] = [
    ("BuggyDecideFallback", "TierWall"),
    ("BuggyDecideUngranted", "NamedDestination"),
    ("BuggyDecideReleaseAbroad", "ReleaseTokenStaysHome"),
    ("BuggyDecideAmbient", "NoAmbient"),
    ("BuggyAnonymousCarries", "AnonymousCarriesNothing"),
    ("BuggyDecideAnonWrite", "NoAnonymousWriterOrWrite"),
    ("BuggyRefuseButSend", "RefusalSendsNothing"),
    ("BuggyHopKeepsAuth", "HopDropsAuth"),
    ("BuggyHopOnWrite", "NoWriteHop"),
    ("BuggyRelReadAbroad", "ReleaseTokenStaysHome"),
    ("BuggyIdentityDefault", "IdentityNamed"),
];

const WORLD: [&str; 10] = [
    "NextTier",
    "NextOp",
    "ToggleNamed",
    "ToggleGranted",
    "ToggleRelRead",
    "ToggleIdent",
    "ToggleDev",
    "ToggleRel",
    "ToggleAmbient",
    "Reset",
];

fn var(state: &State, name: &str) -> i64 {
    state[name]
}

fn slug_text(tier: i64) -> &'static str {
    match tier {
        0 => "fixture-dev/repo",
        1 => "alabsystems/aterm",
        _ => "fixture-ext/repo",
    }
}

fn access(op: i64) -> Access {
    match op {
        0 => Access::Read,
        1 => Access::WriterRead,
        _ => Access::Write,
    }
}

fn with_access(target: RouteTarget, op: i64) -> RouteRequest {
    match access(op) {
        Access::Read => RouteRequest::read(target),
        Access::WriterRead => RouteRequest::writer_read(target),
        Access::Write => RouteRequest::write(target),
    }
}

/// The requests that stand for `state` on `router`. The identity path is one request whatever
/// the tier, with `--as dev` when `named`. A repository's is the argv literal when `named`;
/// else, with `granted` on the release owner and a router that grants (`grants`: one read
/// from a file), `Router::grant`'s literal; else both `Default` and `@channel`. A `granted`
/// state whose grant is refused (another tier's repository, or a router that does not grant)
/// asserts the refusal is `not_granted` and stands for what `granted` 0 does.
fn requests(state: &State, router: &Router, grants: bool) -> Vec<RouteRequest> {
    let op = var(state, "op");
    if var(state, "ident") == 1 {
        let profile = (var(state, "named") == 1).then(|| "dev".to_string());
        return vec![with_access(RouteTarget::Identity, op).with_as(profile)];
    }
    let text = slug_text(var(state, "tier"));
    let slug = Slug::parse(text).expect("a canonical slug");
    let req = |named: Named| with_access(RouteTarget::Repo(slug.clone()), op).with_named(named);
    if var(state, "named") == 1 {
        let typed = ExplicitTarget::from_argv(OsStr::new(text)).expect("the CLI's literal");
        return vec![req(Named::Literal(typed))];
    }
    if var(state, "granted") == 1 {
        match router.grant(CALLER, &slug) {
            Ok(target) => {
                assert!(
                    grants && var(state, "tier") == 1,
                    "{state:?}: a grant for {text}"
                );
                assert_eq!(target.provenance(), Provenance::Granted);
                assert_eq!(target.granted_to(), Some(CALLER));
                return vec![req(Named::Literal(target))];
            }
            Err(error) => {
                assert!(
                    !grants || var(state, "tier") != 1,
                    "{state:?}: {}",
                    error.why
                );
                assert_eq!(
                    error.sub,
                    crate::error::Sub::Policy(RefusalCode::NotGranted),
                    "{state:?}: {}",
                    error.why
                );
            }
        }
    }
    vec![
        req(Named::Default),
        req(Named::Target("channel".to_string())),
    ]
}

struct World {
    _root: aterm_tempfile::TempDir,
    router: Router,
    /// The same agh.toml through `config::validate` (origin `Text`); `None` over the vault.
    text_router: Option<Router>,
}

/// The canonical agh.toml with the grant, and with `relread` the release profile at the head of
/// the release owner's read chain, read back by `config::load` (origin `File`).
fn world(dev: bool, rel: bool, relread: bool) -> World {
    let root = crate::test_fixtures::scratch_dir("wall");
    let base = root.path();
    let mut text = crate::test_fixtures::canonical_text(base);
    if relread {
        text = text.replacen(
            "read = [\"dev\", \"anonymous\"]",
            "read = [\"release\", \"dev\", \"anonymous\"]",
            1,
        );
    }
    text.push_str(WRITERS);
    let dirs = Dirs::under(base);
    write_config(&dirs, &text);
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
    assert_eq!(validated.origin(), config::ConfigOrigin::File);
    let parsed = config::parse(&text).expect("the world's agh.toml parses");
    let from_text =
        config::validate(&parsed, base, &config::file_id).expect("the world's agh.toml validates");
    assert_eq!(from_text.origin(), config::ConfigOrigin::Text);
    World {
        _root: root,
        router: Router::from_validated(validated),
        text_router: Some(Router::from_validated(from_text)),
    }
}

/// The router a call with no agh.toml builds (`vault::view`, as `vault::adopt` does once each
/// login is recorded): the vault's dev token (login `fixture-dev`, so the dev owner is the
/// model's tier 0) when `dev`, its release token when `rel`, and a record holding both logins,
/// as a machine whose tokens were adopted before has. Adoption's rows hold no chain that
/// `relread` names, so it builds the same world either way.
fn vault_world(dev: bool, rel: bool, relread: bool) -> World {
    let _ = relread;
    let root = crate::test_fixtures::scratch_dir("wall-vault");
    let dirs = Dirs::under(root.path());
    std::fs::create_dir_all(&dirs.home).expect("the home");
    aterm_uds::private_file::create_private_dir(&dirs.home.join(".secrets"))
        .expect("the vault directory");
    let tokens = [
        (Slot::Dev, "dev", "fixture-dev", dev),
        (Slot::Release, "release", "fixture-release-bot", rel),
    ];
    let mut learned = Vec::new();
    for (slot, seed, login, present) in tokens {
        let token = agh_fake::token(seed);
        if present {
            setup_file(&slot.path(&dirs.home), format!("{token}\n").as_bytes());
        }
        learned.push((slot, identity::fingerprint(token.as_bytes()), login));
    }
    let rows: Vec<(Slot, &str, &str)> = learned
        .iter()
        .map(|(slot, fingerprint, login)| (*slot, fingerprint.as_str(), *login))
        .collect();
    vault::record_for_tests(&dirs, &rows).expect("the record");
    let adoption = vault::view(&dirs, &MapEnv::new()).expect("no agh.toml: the vault adopts");
    World {
        _root: root,
        router: adoption.router().expect("the adopted rows validate"),
        text_router: None,
    }
}

/// Every world `make` builds, each built once: by `(dev, rel, relread)`.
type Worlds = BTreeMap<(bool, bool, bool), World>;

fn worlds(make: fn(bool, bool, bool) -> World) -> Worlds {
    let mut built = BTreeMap::new();
    for dev in [false, true] {
        for rel in [false, true] {
            for relread in [false, true] {
                built.insert((dev, rel, relread), make(dev, rel, relread));
            }
        }
    }
    built
}

fn world_of<'a>(worlds: &'a Worlds, state: &State) -> &'a World {
    let key = (
        var(state, "dev") == 1,
        var(state, "rel") == 1,
        var(state, "relread") == 1,
    );
    &worlds[&key]
}

fn env(amb: bool) -> MapEnv {
    if amb {
        MapEnv::new().with("GH_TOKEN", &agh_fake::token("ambient"))
    } else {
        MapEnv::new()
    }
}

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

fn with_decision(state: &State, (phase, used): (i64, i64)) -> State {
    let mut next = state.clone();
    next.insert("phase", phase);
    next.insert("used", used);
    next
}

fn successor(model: &Model, state: &State, action: &str) -> Option<State> {
    let mut next = model.successors(action, state);
    assert!(next.len() <= 1, "{action} from {state:?}: {next:?}");
    next.pop()
}

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

type Key = [i64; 9];

fn key(state: &State) -> Key {
    [
        var(state, "tier"),
        var(state, "op"),
        var(state, "named"),
        var(state, "granted"),
        var(state, "relread"),
        var(state, "ident"),
        var(state, "dev"),
        var(state, "rel"),
        var(state, "amb"),
    ]
}

#[test]
fn the_real_router_and_hop_conform_to_the_route_wall() {
    conform(&agh_route_wall_model(), world, true);
}

/// The same wall over vault adoption's rows: no route path of adoption's sends what the laws
/// forbid, and every mutant's step differs from the adopted router's.
#[test]
fn the_adopted_router_conforms_to_the_route_wall() {
    let model = interp::with_consts(&agh_route_wall_model(), &[("Vault", 1)]);
    conform(&model, vault_world, false);
}

/// `model`'s Decide against `Router::decide` over each world `make` builds, its Hop against
/// `hop::rebuild`, and its mutants against both. `grants` says whether the worlds' routers
/// grant (`Router::grant` over a file-made `Validated`).
fn conform(model: &Model, make: fn(bool, bool, bool) -> World, grants: bool) {
    std::hint::black_box(crate::policy::route_wall_environment as fn());
    let states = interp::reachable_states(model);
    let worlds = worlds(make);
    let mut ev = StepEvidence::new(MACHINE);

    let mut real: BTreeMap<Key, (i64, i64)> = BTreeMap::new();

    for state in states.iter().filter(|s| var(s, "phase") == 0) {
        let world = world_of(&worlds, state);
        let amb = env(var(state, "amb") == 1);
        let expected = successor(model, state, "Decide").expect("Decide fires in phase 0");
        let mut projected = BTreeSet::new();
        for req in requests(state, &world.router, grants) {
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
        match successor(model, state, "Hop") {
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

    let buggy = interp::with_buggy(model, 1);
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

    let forged: BTreeMap<(bool, bool, bool), Router> = worlds
        .iter()
        .map(|(key, world)| (*key, forged_floor(world)))
        .collect();
    // The literals judged on the forged router: (argv, minted grant).
    let mut literals = (0_usize, 0_usize);
    for state in states
        .iter()
        .filter(|s| var(s, "phase") == 0 && var(s, "tier") == 1)
    {
        let at = (
            var(state, "dev") == 1,
            var(state, "rel") == 1,
            var(state, "relread") == 1,
        );
        let router = &forged[&at];
        let amb = env(var(state, "amb") == 1);
        for req in requests(state, router, grants) {
            let decision = decide(router, &req, &amb, Some(&mut ev));
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
                // A literal (the argv's, or a minted grant) meets the forged dev rule for the
                // release owner, which is `TierMismatch`; a default or `@target` write is
                // refused before the tier is judged, for want of a literal.
                if let Named::Literal(literal) = &req.named {
                    if literal.provenance() == Provenance::Granted {
                        literals.1 += 1;
                    } else {
                        literals.0 += 1;
                    }
                    assert!(
                        matches!(
                            &decision,
                            Decision::Refuse(refusal) if refusal.code == RefusalCode::TierMismatch
                        ),
                        "{state:?}: {decision:?}"
                    );
                }
            }
        }
    }
    // A router that grants (one read from a file) mints grants there too.
    assert!(
        literals.0 > 0 && (literals.1 > 0) == grants,
        "forged-floor literals judged: {literals:?}"
    );

    // Outside the model: the same agh.toml through `config::validate` (origin `Text`) grants
    // nothing, so its WriterReads and Writes on the release owner project as `granted` 0.
    for state in states.iter().filter(|s| {
        var(s, "phase") == 0
            && var(s, "tier") == 1
            && var(s, "op") > 0
            && var(s, "ident") == 0
            && var(s, "named") == 0
    }) {
        let Some(text_router) = &world_of(&worlds, state).text_router else {
            continue;
        };
        let slug = Slug::parse(slug_text(1)).expect("the release slug");
        let refused = text_router
            .grant(CALLER, &slug)
            .expect_err("a text-made agh.toml grants nothing");
        assert_eq!(
            refused.sub,
            crate::error::Sub::Policy(RefusalCode::NotGranted)
        );
        let mut ungranted = state.clone();
        ungranted.insert("granted", 0);
        let amb = env(var(state, "amb") == 1);
        for req in requests(&ungranted, text_router, false) {
            let decision = decide(text_router, &req, &amb, None);
            assert_eq!(
                project_decision(&decision),
                real[&key(&ungranted)],
                "{state:?}: {decision:?}"
            );
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
    assert_eq!(waived.len(), 21);
    assert_eq!(waived, expected);
    let every: BTreeSet<&str> = model.actions.iter().map(|a| a.name).collect();
    assert_eq!(every.len(), 23);
    assert_eq!(
        bound.union(&waived).copied().collect::<BTreeSet<_>>(),
        every
    );
}

fn forged_floor(world: &World) -> Router {
    let mut validated = world.router.validated.clone().unwrap_or_else(|| Validated {
        profiles: BTreeMap::new(),
        owners: BTreeMap::new(),
        default_read: vec![ChainStep::Anonymous],
        targets: BTreeMap::new(),
        vault: false,
        origin: config::ConfigOrigin::Text,
        writers: Vec::new(),
    });
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
