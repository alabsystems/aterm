// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! AghEnsureOnce Tier-1 conformance (design §17.2): `Releases::create`'s four steps
//! (`create_step`, `send_create`, `settle_create`, `discover`) against the model, state by
//! state and edge by edge, over agh-fake, with the six mutants as negative controls; and
//! `racing_creates_post_once`, which holds the code to the model's atomic `Begin`.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use agh_fake::{FakeGithub, Fault, Match, ReleaseSpec, RepoSpec};
use aterm_models_agh::agh_ensure_once_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use aterm_spec::xref::{self, StepEvidence};

use super::*;
use crate::client::GithubBuilder;
use crate::config::{self, Dirs};
use crate::fx::Fx;
use crate::identity::MapEnv;
use crate::journal::{Journal, JournalRecord};
use crate::policy::{RouteRequest, RouteTarget, Router};
use crate::retry::RetryPolicy;
use crate::test_fixtures::{canonical_text, setup_file, toml_path, write_config};
use crate::wire::Endpoints;
use crate::wire::http::HttpTransport;

const MACHINE: &str = "AghEnsureOnce";
/// The simulated clock's start (`tests/common.rs`'s, which an in-crate test cannot reach).
const SIM_START: u64 = 1_760_000_000;
/// `tests/common.rs`'s `MAIN_SHA`.
const MAIN_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
/// `tests/common.rs`'s `WRITERS`.
const WRITERS: &str =
    "\n[[writers]]\ncaller = \"fixture-caller\"\nrepos = [\"alabsystems/aterm\"]\n";
const TAG: &str = "v1";
const TARGET: &str = "main";
const NOTES: &str = "notes";
/// agh-fake's 422 body A: a published release holds the tag.
const BODY_A: &str = r#"{"message":"Validation Failed","errors":[{"resource":"Release","code":"already_exists","field":"tag_name"}]}"#;

/// The world's actions no agh function performs: each is a waiver.
const WORLD: [&str; 9] = [
    "NotSent",
    "AnswerCreated",
    "AnswerRefused",
    "LandedLost",
    "FailedLost",
    "Reveal",
    "Rediscover",
    "Crash",
    "LandedAfterCrash",
];

/// Each mutant, the law it breaks, and the lawful action its state also enables.
const MUTANTS: [(&str, &str, &str); 6] = [
    ("BuggySendWithoutIntent", "SendNeedsIntent", "Begin"),
    ("BuggyRepostOnUnknown", "OnePostPerIntent", "Discover"),
    ("BuggyRefusedStaysOpen", "RefusedCloses", "Settle"),
    ("BuggyLostIsCreated", "CreatedNeedsObject", "Settle"),
    ("BuggyDiscoverAsReader", "DiscoveryIsWriterRead", "Discover"),
    ("BuggyNoneCloses", "NoneFoundStaysOpen", "Discover"),
];

/// The four actions the code binds.
const BOUND: [&str; 4] = ["Begin", "Discover", "Send", "Settle"];

/// The answers the world gives the one POST.
const OUTCOMES: [&str; 5] = [
    "AnswerCreated",
    "AnswerRefused",
    "LandedLost",
    "FailedLost",
    "NotSent",
];

fn var(state: &State, name: &str) -> i64 {
    state[name]
}

fn enabled(model: &Model, action: &str, state: &State) -> bool {
    !model.successors(action, state).is_empty()
}

fn successor(model: &Model, state: &State, action: &str) -> State {
    let mut next = model.successors(action, state);
    assert_eq!(next.len(), 1, "{action} from {state:?}: {next:?}");
    next.pop().expect("one successor")
}

/// The draft request every state is judged with: it agrees with every intent a world holds.
fn request() -> DraftRequest {
    DraftRequest {
        target_commitish: Some(TARGET.to_string()),
        body: NOTES.to_string(),
        ..DraftRequest::new(TAG)
    }
}

/// What a world is built from: `(intent, settled, found, drafts, visible, begun, runs > 0)`.
type Key = (i64, i64, i64, i64, i64, i64, bool);

fn key(state: &State) -> Key {
    (
        var(state, "intent"),
        var(state, "settled"),
        var(state, "found"),
        var(state, "drafts"),
        var(state, "visible"),
        var(state, "begun"),
        var(state, "runs") > 0,
    )
}

/// One repository and its journal, holding what a state says.
struct World {
    text: String,
    slug: Slug,
    journal: Journal,
}

/// The fake, the scratch root, the router read from its agh.toml, and the client.
struct Setup {
    _root: aterm_tempfile::TempDir,
    fake: FakeGithub,
    dirs: Dirs,
    router: Router,
    gh: Github,
    worlds: Cell<u32>,
}

/// The canonical agh.toml with a dev-tier `reader` profile (login `fixture-reader`) at the
/// head of the `fixture-dev` read chain, and the `[[writers]]` grant.
fn config_text(root: &std::path::Path) -> String {
    let canonical = canonical_text(root);
    let chain = "read = [\"dev\"]\nwrite = \"dev\"";
    assert!(canonical.contains(chain), "the canonical fixture-dev row");
    let reader = format!(
        "[profiles.reader]\ntoken_file = \"{}/tokens/reader.token\"\ntier = \"dev\"\nlogin = \
         \"fixture-reader\"\n\n",
        toml_path(root)
    );
    format!(
        "{reader}{}{WRITERS}",
        canonical.replacen(chain, "read = [\"reader\", \"dev\"]\nwrite = \"dev\"", 1)
    )
}

impl Setup {
    fn new(label: &str) -> Setup {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("agh-{label}-"))
            .tempdir()
            .expect("a scratch root");
        let base = root.path();
        let fake = FakeGithub::start();
        for (dir, name, seed, login) in [
            ("tokens", "dev.token", "dev", "fixture-dev-bot"),
            ("tokens", "reader.token", "reader", "fixture-reader"),
            ("secrets", "release.token", "release", "fixture-release-bot"),
        ] {
            let token = agh_fake::token(seed);
            fake.add_token(&token, login);
            let dir = base.join(dir);
            aterm_uds::private_file::create_private_dir(&dir).expect("a token directory");
            setup_file(&dir.join(name), format!("{token}\n").as_bytes());
        }
        let dirs = Dirs::under(base);
        write_config(&dirs, &config_text(base));
        let validated = config::load(&dirs)
            .expect("the conformance agh.toml loads")
            .expect("the conformance agh.toml is there");
        let router = Router::from_validated(validated);
        let ports = fake.ports();
        let endpoints = Endpoints::loopback(ports.api, ports.uploads, ports.web, ports.cdn);
        let gh = client(&router, &dirs, endpoints);
        Setup {
            _root: root,
            fake,
            dirs,
            router,
            gh,
            worlds: Cell::new(0),
        }
    }

    /// A client of the same router and journal whose endpoints name a port that was bound
    /// and closed: its POST never leaves the host.
    fn dead_client(&self) -> Github {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().expect("its address").port();
        drop(listener);
        client(
            &self.router,
            &self.dirs,
            Endpoints::loopback(port, port, port, port),
        )
    }

    /// A fresh repository `fixture-dev/w{n}` holding what `state` says (plan §9's table).
    fn world(&self, state: &State) -> World {
        let n = self.worlds.get();
        self.worlds.set(n + 1);
        let text = format!("fixture-dev/w{n}");
        self.fake.add_repo(
            &text,
            RepoSpec::private()
                .push("fixture-dev-bot")
                .pull("fixture-reader"),
        );
        self.fake.set_branch(&text, "main", MAIN_SHA);
        let slug = Slug::parse(&text).expect("a world's slug");
        let drafts = var(state, "drafts");
        assert!(
            drafts <= 1,
            "a lawful state holds at most one draft: {state:?}"
        );
        if var(state, "visible") == 0 {
            self.fake.set_listing_lag(&text, true);
        }
        let draft = (drafts == 1).then(|| {
            let mut spec = ReleaseSpec::draft(TAG, "2026-10-01T00:00:00Z");
            spec.target_commitish = TARGET.to_string();
            spec.body = Some(NOTES.to_string());
            self.fake.add_release(&text, spec)
        });
        let journal = self.gh.journal(&slug).expect("the world's journal");
        let permit = self.permit(&self.gh, &slug);
        let releases = self.gh.releases(&slug);
        let call = releases.create_call(&request()).expect("the create call");
        let draft_of = |after: Option<IntentId>| {
            self.gh
                .intent_draft(WriteOp::ReleaseCreate, &slug, &call, &permit, TAG)
                .expect("the intent's draft")
                .with_target(Some(TARGET))
                .with_body_digest(Some(Sha256Digest::of(NOTES.as_bytes()).to_string()))
                .with_after(after)
        };
        let mut first = None;
        if var(state, "begun") == 2 {
            let id = journal.begin(&draft_of(None)).expect("the first intent");
            journal
                .settle(&id, &Settled::Refused { status: 422 })
                .expect("the first intent is refused");
            first = Some(id);
        }
        if var(state, "intent") >= 1 {
            let id = journal.begin(&draft_of(first)).expect("the intent");
            let made = || draft.expect("CreatedNeedsObject: a created intent has its draft");
            let unknown = Settled::Unknown {
                why: "GitHub answered 502".to_string(),
            };
            match (var(state, "settled"), var(state, "found")) {
                (1, _) => journal.settle(&id, &Settled::NeverSent),
                (2, _) => journal.settle(&id, &Settled::Refused { status: 422 }),
                (3, 1) => journal
                    .settle(&id, &unknown)
                    .and_then(|()| journal.observe(&id, Some(made()))),
                (3, _) => journal.settle(
                    &id,
                    &Settled::Created {
                        id: made(),
                        url: format!("https://github.com/{text}/releases/tag/{TAG}"),
                    },
                ),
                (4, _) if var(state, "runs") == 0 => journal.settle(&id, &unknown),
                _ => Ok(()),
            }
            .expect("the world's records");
            if var(state, "found") == 2 {
                journal.observe(&id, None).expect("an Observed none");
            }
        }
        World {
            text,
            slug,
            journal,
        }
    }

    /// The dev writer's permit for `slug` from `gh`.
    fn permit(&self, gh: &Github, slug: &Slug) -> Permit {
        gh.permit(&RouteRequest::write(RouteTarget::Repo(slug.clone())))
            .unwrap_or_else(|e| panic!("no permit: {}", e.why))
    }

    /// The requests `seen()` holds from `mark` on, on `world`'s repository.
    fn seen_since(&self, mark: usize, world: &World) -> Vec<agh_fake::Seen> {
        let prefix = format!("/repos/{}/", world.text);
        self.fake
            .seen()
            .into_iter()
            .skip(mark)
            .filter(|seen| seen.path_and_query.starts_with(&prefix))
            .collect()
    }
}

fn client(router: &Router, dirs: &Dirs, endpoints: Endpoints) -> Github {
    GithubBuilder::new(router.clone())
        .transport(Arc::new(HttpTransport::direct(Arc::new(MapEnv::new()))))
        .endpoints(endpoints)
        .env(Arc::new(MapEnv::new()))
        .fx(Arc::new(Fx::simulated(SIM_START)))
        .caller("test")
        .retry(RetryPolicy::NONE)
        .journal(dirs.clone())
        .build()
}

/// The world's open intent: the newest.
fn open_intent(journal: &Journal) -> Intent {
    journal
        .open()
        .expect("the journal reads")
        .into_iter()
        .max_by(|a, b| a.id.cmp(&b.id))
        .expect("an open intent")
}

/// What one Send and its Settle did.
struct Sent {
    sent: i64,
    drafts: i64,
    settled: (i64, i64),
    open: bool,
}

/// A fresh world for `pre` (a state where Send is enabled), its POST answered as `outcome`
/// says, then settled: each step in its own evidence window.
fn send_and_settle(setup: &Setup, ev: &mut StepEvidence, pre: &State, outcome: &str) -> Sent {
    let world = setup.world(pre);
    let intent = open_intent(&world.journal);
    let dead;
    let gh = if outcome == "NotSent" {
        dead = setup.dead_client();
        &dead
    } else {
        &setup.gh
    };
    let fault = match outcome {
        "AnswerRefused" => Some(Fault::Status {
            status: 422,
            body: BODY_A.to_string(),
            headers: Vec::new(),
        }),
        "LandedLost" => Some(Fault::LandThenClose),
        "FailedLost" => Some(Fault::Close),
        _ => None,
    };
    if let Some(fault) = fault {
        setup.fake.fault(
            Match::api(&format!("/repos/{}/releases", world.text)).method("POST"),
            fault,
        );
    }
    let permit = setup.permit(gh, &world.slug);
    let releases = gh.releases(&world.slug);
    let call = releases.create_call(&request()).expect("the create call");
    let result = ev.step("Send", || releases.send_create(&call, &permit));
    let drafts = i64::try_from(setup.fake.releases_of(&world.text).len()).expect("a count");
    let settled = ev
        .step("Settle", || {
            releases.settle_create(&result, &intent.id, &world.journal)
        })
        .expect("the settle is journaled");
    let open = world
        .journal
        .open()
        .expect("the journal reads")
        .iter()
        .any(|o| o.id == intent.id);
    Sent {
        sent: project_sent(&result),
        drafts,
        settled: project_settled(&settled),
        open,
    }
}

/// A fresh world for `state` (a state where Discover is enabled), then `discover` in its
/// window: the projection, whether the Observed record found something, and the logins the
/// step's requests signed in as.
fn discover_in(
    setup: &Setup,
    ev: &mut StepEvidence,
    state: &State,
) -> ((i64, i64, i64), bool, Vec<Option<String>>) {
    let world = setup.world(state);
    let intent = open_intent(&world.journal);
    let releases = setup.gh.releases(&world.slug);
    let mark = setup.fake.seen().len();
    let (before, _) = world.journal.read().expect("the journal reads");
    let found = ev
        .step("Discover", || releases.discover(&intent, &world.journal))
        .expect("discovery runs");
    let (records, _) = world.journal.read().expect("the journal reads");
    assert_eq!(
        records.len(),
        before.len() + 1,
        "discovery appends one record at {state:?}: {records:?}"
    );
    let Some((
        _,
        JournalRecord::Observed {
            intent: observed,
            found: seen,
            ..
        },
    )) = records.last()
    else {
        panic!("discovery appends an Observed record: {records:?}");
    };
    assert_eq!(
        observed, &intent.id,
        "the Observed record is the open intent's"
    );
    let seen = seen.is_some();
    let listing = format!("/repos/{}/releases?", world.text);
    let requests = setup.seen_since(mark, &world);
    assert!(
        requests
            .iter()
            .any(|s| s.path_and_query.starts_with(&listing)
                && s.as_login.as_deref() == Some("fixture-dev-bot")),
        "discovery's listing, as the writer, at {state:?}: {requests:?}"
    );
    let logins = requests.into_iter().map(|s| s.as_login).collect();
    (project_discovered(&found), seen, logins)
}

/// The answer that left `state`'s `answer` (a state after Send and an answer): the outcome
/// and the state Send was enabled in.
fn before_the_answer(state: &State) -> (State, &'static str) {
    let outcome = match (var(state, "answer"), var(state, "drafts")) {
        (1, _) => "AnswerCreated",
        (2, _) => "AnswerRefused",
        (3, 1) => "LandedLost",
        (3, _) => "FailedLost",
        other => panic!("no answer at {state:?}: {other:?}"),
    };
    let mut pre = state.clone();
    pre.insert("sent", 0);
    pre.insert("answer", 0);
    if matches!(outcome, "AnswerCreated" | "LandedLost") {
        pre.insert("drafts", var(state, "drafts") - 1);
    }
    (pre, outcome)
}

#[test]
fn the_release_protocol_conforms_to_ensure_once() {
    std::hint::black_box(ensure_once_environment as fn());
    let model = agh_ensure_once_model();
    let states = interp::reachable_states(&model);
    let setup = Setup::new("ensure-once");
    let mut ev = StepEvidence::new(MACHINE);

    // Begin, state by state, each world built once per key.
    let mut worlds: BTreeMap<Key, World> = BTreeMap::new();
    let mut begins = 0_usize;
    for state in states.iter().filter(|s| var(s, "begun") <= 1) {
        let world = worlds
            .entry(key(state))
            .or_insert_with(|| setup.world(state));
        let releases = setup.gh.releases(&world.slug);
        let step = ev
            .step("Begin", || releases.create_step(&request(), &world.journal))
            .unwrap_or_else(|e| panic!("create_step at {state:?}: {}", e.why));
        let begin = enabled(&model, "Begin", state);
        assert_eq!(
            project_create_step(&step),
            i64::from(begin),
            "{state:?}: {step:?}"
        );
        begins += usize::from(begin);
    }
    assert!(begins > 0, "no Begin state was checked");

    // Send and Settle, edge by edge.
    let mut edges = 0_usize;
    for state in states.iter().filter(|s| enabled(&model, "Send", s)) {
        for outcome in OUTCOMES {
            let run = send_and_settle(&setup, &mut ev, state, outcome);
            let after = if outcome == "NotSent" {
                successor(&model, state, "NotSent")
            } else {
                let sent = successor(&model, state, "Send");
                assert_eq!(run.sent, var(&sent, "sent"), "{outcome} at {state:?}");
                let answered = successor(&model, &sent, outcome);
                assert_eq!(
                    run.drafts,
                    var(&answered, "drafts"),
                    "{outcome} at {state:?}"
                );
                successor(&model, &answered, "Settle")
            };
            if outcome == "NotSent" {
                assert_eq!(run.sent, var(&after, "sent"), "{outcome} at {state:?}");
                assert_eq!(run.drafts, var(&after, "drafts"), "{outcome} at {state:?}");
            }
            assert_eq!(
                run.settled,
                (var(&after, "settled"), var(&after, "intent")),
                "{outcome} at {state:?}"
            );
            assert_eq!(
                run.open,
                var(&after, "intent") == 1,
                "{outcome} at {state:?}"
            );
            edges += 1;
        }
    }
    assert!(edges >= OUTCOMES.len(), "no Send edge was checked");

    // Discover, state by state.
    let mut discovered = 0_usize;
    for state in states.iter().filter(|s| enabled(&model, "Discover", s)) {
        let (projected, found, logins) = discover_in(&setup, &mut ev, state);
        let next = successor(&model, state, "Discover");
        assert_eq!(
            projected,
            (
                var(&next, "found"),
                var(&next, "intent"),
                var(&next, "settled")
            ),
            "{state:?}"
        );
        assert_eq!(found, var(state, "visible") == 1, "{state:?}");
        assert!(
            !logins
                .iter()
                .any(|l| l.as_deref() == Some("fixture-reader")),
            "discovery read as the reader at {state:?}: {logins:?}"
        );
        discovered += 1;
    }
    assert!(discovered > 0, "no Discover state was checked");

    // The negative controls.
    let buggy = interp::with_buggy(&model, 1);
    for (mutant, law, lawful) in MUTANTS {
        let state = states
            .iter()
            .find(|s| enabled(&buggy, mutant, s) && enabled(&model, lawful, s))
            .unwrap_or_else(|| panic!("no state enables both {mutant} and {lawful}"));
        let wrong = successor(&buggy, state, mutant);
        let right = successor(&model, state, lawful);
        assert_ne!(wrong, right, "{mutant} at {state:?}");
        assert!(
            model.check_invariant(law, &right),
            "{law} fails on {lawful} at {state:?}"
        );
        assert!(
            !buggy.check_invariant(law, &wrong),
            "{mutant} keeps {law} at {state:?}"
        );
        match lawful {
            "Begin" => {
                let world = setup.world(state);
                let releases = setup.gh.releases(&world.slug);
                let step = ev
                    .step("Begin", || releases.create_step(&request(), &world.journal))
                    .expect("create_step");
                assert_eq!(project_create_step(&step), 1, "{mutant} at {state:?}");
            }
            "Settle" => {
                let (pre, outcome) = before_the_answer(state);
                let run = send_and_settle(&setup, &mut ev, &pre, outcome);
                assert_eq!(
                    run.settled,
                    (var(&right, "settled"), var(&right, "intent")),
                    "{mutant} at {state:?}"
                );
            }
            _ => {
                let (projected, _, logins) = discover_in(&setup, &mut ev, state);
                assert_eq!(
                    projected,
                    (
                        var(&right, "found"),
                        var(&right, "intent"),
                        var(&right, "settled")
                    ),
                    "{mutant} at {state:?}"
                );
                if mutant == "BuggyDiscoverAsReader" {
                    assert!(
                        logins
                            .iter()
                            .all(|l| l.as_deref() == Some("fixture-dev-bot")),
                        "{mutant}: discovery signs in as the writer: {logins:?}"
                    );
                }
            }
        }
    }

    // The audit.
    let audit = ev.audit();
    assert!(audit.is_complete(), "{audit:?}");
    let bound_set: BTreeSet<&str> = BOUND.into_iter().collect();
    assert_eq!(audit.actions, bound_set);
    let refined: BTreeSet<&str> = xref::refinements()
        .filter(|r| r.machine == MACHINE)
        .map(|r| r.action)
        .collect();
    assert_eq!(refined, bound_set);
    let waived: BTreeSet<&str> = xref::waivers()
        .filter(|w| w.machine == MACHINE)
        .map(|w| w.action)
        .collect();
    let expected: BTreeSet<&str> = WORLD
        .into_iter()
        .chain(MUTANTS.iter().map(|(mutant, _, _)| *mutant))
        .collect();
    assert_eq!(waived.len(), 15);
    assert_eq!(waived, expected);
    let every: BTreeSet<&str> = model.actions.iter().map(|a| a.name).collect();
    assert_eq!(every.len(), 19);
    assert_eq!(
        refined.union(&waived).copied().collect::<BTreeSet<_>>(),
        every
    );
}

/// The model's `Begin` is one atomic step; two processes that both judged `Begin` for one tag
/// are held to it by `begin`'s re-judging under its lock.
#[test]
fn racing_creates_post_once() {
    let setup = Setup::new("ensure-race");
    let mut empty: State = State::new();
    for name in ["intent", "settled", "found", "drafts", "begun", "runs"] {
        empty.insert(name, 0);
    }
    empty.insert("visible", 1);
    let world = setup.world(&empty);
    let ports = setup.fake.ports();
    let second = client(
        &setup.router,
        &setup.dirs,
        Endpoints::loopback(ports.api, ports.uploads, ports.web, ports.cdn),
    );
    let journal_two = second.journal(&world.slug).expect("the second journal");
    let one = setup.gh.releases(&world.slug);
    let two = second.releases(&world.slug);
    let req = request();
    for (releases, journal) in [(&one, &world.journal), (&two, &journal_two)] {
        let step = releases
            .create_step(&req, journal)
            .expect("create_step on the empty journal");
        assert_eq!(step, CreateStep::Begin { after: None });
    }
    let permit_one = setup.permit(&setup.gh, &world.slug);
    let first = one
        .create(&permit_one, &req, &world.journal)
        .expect("the first create");
    let Outcome::Done(made) = first else {
        panic!("the first create is done: {first:?}");
    };
    assert_eq!(made.effect, Effect::Created);
    let mark = setup.fake.seen().len();
    let permit_two = setup.permit(&second, &world.slug);
    let call = two.create_call(&req).expect("the create call");
    let draft = second
        .intent_draft(WriteOp::ReleaseCreate, &world.slug, &call, &permit_two, TAG)
        .expect("the second draft")
        .with_target(Some(TARGET))
        .with_body_digest(Some(Sha256Digest::of(NOTES.as_bytes()).to_string()))
        .with_after(None);
    let refused = journal_two
        .begin(&draft)
        .expect_err("the second begin is refused");
    assert_eq!(refused.kind, Kind::OutcomeUnknown, "{refused:?}");
    let first_intent = open_or_latest(&world.journal);
    assert_eq!(
        refused.sub,
        Sub::IntentOpen {
            intent: first_intent.to_string()
        },
        "{refused:?}"
    );
    assert!(
        setup.seen_since(mark, &world).is_empty(),
        "the refused begin sent nothing"
    );
    let posts = setup
        .fake
        .seen()
        .into_iter()
        .filter(|s| s.method == "POST")
        .count();
    assert_eq!(posts, 1);
    assert_eq!(setup.fake.releases_of(&world.text).len(), 1);
    let (records, _) = world.journal.read().expect("the journal reads");
    let intents = records
        .iter()
        .filter(|(_, r)| matches!(r, JournalRecord::Intent(_)))
        .count();
    assert_eq!(intents, 1, "{records:?}");
    let again = two
        .create(&permit_two, &req, &journal_two)
        .expect("the second create, run again");
    let Outcome::Done(converged) = again else {
        panic!("the rerun converges: {again:?}");
    };
    assert_eq!(converged.effect, Effect::Converged);
    assert_eq!(converged.object.id, made.object.id);
}

/// The id of the journal's latest `ReleaseCreate` intent of the tag.
fn open_or_latest(journal: &Journal) -> IntentId {
    journal
        .latest(WriteOp::ReleaseCreate, TAG)
        .expect("the journal reads")
        .expect("an intent")
        .intent
        .id
}
