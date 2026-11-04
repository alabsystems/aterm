// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The client's write pipeline (plan §7, body 5 and 6): pacing, the retry classes and their
//! guard, the sleep budget, the rate-book switch, login binding, the settle table, the
//! one-shot steps and the request digest. Over agh-fake's loopback server and
//! `Fx::simulated(SIM_START)`, with an in-memory `IntentStore`.

use std::ffi::OsStr;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use agh_fake::{FakeGithub, Fault, Match, RepoSpec};

use super::{Answered, Call, CallBody, Github, GithubBuilder};
use crate::config::{self, Dirs};
use crate::error::{Error, HttpRef, IdentityRef, Kind, Result, SideEffect, Sub};
use crate::fx::{Fx, canon};
use crate::github::How;
use crate::identity::{self, InlineToken, MapEnv, Profile};
use crate::journal::{Intent, IntentDraft, IntentId, IntentStore, IntentView, Settled};
use crate::policy::{ExplicitTarget, Named, Permit, RouteRequest, RouteTarget, Router, Tier};
use crate::rate::{RateBook, RateSnapshot};
use crate::retry::{Idempotency, RetryPolicy};
use crate::slug::Slug;
use crate::wire::http::HttpTransport;
use crate::wire::scripted::ScriptedTransport;
use crate::wire::{Endpoints, Method, OriginRole, Transport, WireBody, WireKind};
use crate::write::{Outcome, WriteOp, settle_table};

/// The simulated clock's start (Unix seconds): `tests/common.rs`'s value, which this crate's
/// graph does not hold (plan §4.8).
const SIM_START: u64 = 1_760_000_000;

const APP: &str = "fixture-dev/app";
const ATERM: &str = "alabsystems/aterm";

fn scratch() -> aterm_tempfile::TempDir {
    crate::test_fixtures::scratch_dir("write")
}

fn slug(text: &str) -> Slug {
    Slug::parse(text).expect("a slug")
}

/// `{token}\n` at `dir/name`, 0600 in a 0700 directory.
fn write_token(dir: &Path, name: &str, token: &str) {
    aterm_uds::private_file::create_private_dir(dir).expect("the token directory");
    let path = dir.join(name);
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{token}\n").as_bytes()))
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

/// The canonical config under `root`, with both token files written.
fn canonical(root: &Path) -> Router {
    write_token(&root.join("tokens"), "dev.token", &agh_fake::token("dev"));
    write_token(
        &root.join("secrets"),
        "release.token",
        &agh_fake::token("release"),
    );
    let root_text = root
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let text = include_str!("../tests/fixtures/canonical.toml").replace("{root}", &root_text);
    let parsed = config::parse(&text).expect("the canonical config parses");
    let validated =
        config::validate(&parsed, root, &config::file_id).expect("the canonical config validates");
    Router::from_validated(validated)
}

fn fake() -> FakeGithub {
    let fake = FakeGithub::start();
    fake.add_token(&agh_fake::token("dev"), "fixture-dev-bot");
    fake.add_token(&agh_fake::token("release"), "fixture-release-bot");
    fake.add_repo(APP, RepoSpec::private().push("fixture-dev-bot"));
    fake.add_repo(ATERM, RepoSpec::public().push("fixture-release-bot"));
    fake
}

fn endpoints(fake: &FakeGithub) -> Endpoints {
    let ports = fake.ports();
    Endpoints::loopback(ports.api, ports.uploads, ports.web, ports.cdn)
}

fn builder(router: Router, transport: Arc<dyn Transport>, endpoints: Endpoints) -> GithubBuilder {
    GithubBuilder::new(router)
        .transport(transport)
        .endpoints(endpoints)
        .env(Arc::new(MapEnv::new()))
        .fx(Arc::new(Fx::simulated(SIM_START)))
        .caller("test")
        .retry(RetryPolicy::STANDARD)
}

/// A client of `router` over `fake`, its retry policy `STANDARD`.
fn github(router: Router, fake: &FakeGithub) -> Github {
    builder(
        router,
        Arc::new(HttpTransport::direct(Arc::new(MapEnv::new()))),
        endpoints(fake),
    )
    .build()
}

fn permit_for(gh: &Github, req: &RouteRequest) -> Permit {
    gh.permit(req)
        .unwrap_or_else(|e| panic!("no permit: {}", e.why))
}

fn app_write() -> RouteRequest {
    RouteRequest::write(RouteTarget::Repo(slug(APP)))
}

fn aterm_literal() -> Named {
    Named::Literal(ExplicitTarget::from_argv(OsStr::new(ATERM)).expect("a literal"))
}

fn aterm_write() -> RouteRequest {
    RouteRequest::write(RouteTarget::Repo(slug(ATERM))).with_named(aterm_literal())
}

/// A Converging PATCH of `fixture-dev/app`.
fn patch() -> Call {
    Call::write(
        Method::Patch,
        "/repos/fixture-dev/app",
        app_write(),
        Idempotency::Converging,
        CallBody::Json(b"{\"description\":\"x\"}".to_vec()),
    )
}

/// A one-shot POST of a tag reference of `fixture-dev/app`.
fn post_ref() -> Call {
    Call::write(
        Method::Post,
        "/repos/fixture-dev/app/git/refs",
        app_write(),
        Idempotency::OneShot,
        CallBody::Json(b"{\"ref\":\"refs/tags/v1\",\"sha\":\"0123\"}".to_vec()),
    )
}

fn answer(status: u16, body: &str) -> Fault {
    Fault::Status {
        status,
        body: body.to_string(),
        headers: Vec::new(),
    }
}

fn patches() -> Match {
    Match::api("/repos/fixture-dev/app").method("PATCH")
}

fn posts() -> Match {
    Match::api("/repos/fixture-dev/app/git/refs").method("POST")
}

fn methods(fake: &FakeGithub) -> Vec<String> {
    fake.seen()
        .iter()
        .map(|s| format!("{} {}", s.method, s.path_and_query))
        .collect()
}

fn answered(status: u16, body: &[u8]) -> Answered {
    Answered {
        status,
        headers: Vec::new(),
        body: body.to_vec(),
        as_: IdentityRef::Anonymous,
        request_id: None,
        etag: None,
        moved: None,
        requests: 1,
        rate: None,
    }
}

/// The ref POST's `created`: the answer's `id` and `url`.
fn created_of(answered: &Answered) -> Option<(u64, String)> {
    let value = answered.json().ok()?;
    let id = value.get("id")?.as_u64()?;
    let url = value.get("url")?.as_str()?.to_string();
    Some((id, url))
}

/// One intent and what was recorded of it.
struct Held {
    intent: Intent,
    settled: Option<Settled>,
}

/// An `IntentStore` in memory. Each `begin` and `settle` also writes its word into `log`,
/// after the methods `watch` saw since the last word, so the log orders the journal's steps
/// against the requests.
#[derive(Default)]
struct MemIntents {
    held: Mutex<Vec<Held>>,
    log: Mutex<Vec<String>>,
    watch: Option<Arc<ScriptedTransport>>,
    logged: Mutex<usize>,
    /// When set, every `settle` fails with this why, and records nothing.
    settle_fails: Option<String>,
}

impl MemIntents {
    fn watching(transport: &Arc<ScriptedTransport>) -> MemIntents {
        MemIntents {
            watch: Some(transport.clone()),
            ..MemIntents::default()
        }
    }

    fn note(&self, word: &str) {
        let mut log = self.log.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(transport) = &self.watch {
            let mut logged = self.logged.lock().unwrap_or_else(PoisonError::into_inner);
            let seen = transport.requests();
            for request in seen.iter().skip(*logged) {
                log.push(request.method.as_str().to_string());
            }
            *logged = seen.len();
        }
        log.push(word.to_string());
    }

    fn log(&self) -> Vec<String> {
        self.note("end");
        let mut log = self
            .log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        log.pop();
        log
    }

    fn settled(&self) -> Vec<Option<Settled>> {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|held| held.settled.clone())
            .collect()
    }
}

fn view(held: &Held) -> IntentView {
    IntentView {
        intent: held.intent.clone(),
        settled: held.settled.clone(),
        observed: Vec::new(),
        retracted: None,
        open: !matches!(
            held.settled,
            Some(Settled::Created { .. } | Settled::Refused { .. } | Settled::NeverSent)
        ),
    }
}

impl IntentStore for MemIntents {
    fn begin(&self, draft: &IntentDraft) -> Result<IntentId> {
        self.note("begin");
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let id = IntentId::parse(&format!("{}-0123456789ab", held.len())).expect("an id");
        held.push(Held {
            intent: Intent {
                id: id.clone(),
                op: draft.op,
                slug: draft.slug.clone(),
                identity: draft.identity.clone(),
                fingerprint: draft.fingerprint.clone(),
                key: draft.key.clone(),
                request: draft.request.clone(),
                target_commitish: draft.target_commitish.clone(),
                body_digest: draft.body_digest.clone(),
                grant: draft.grant.clone(),
                at_secs: SIM_START,
            },
            settled: None,
        });
        Ok(id)
    }

    fn settle(&self, id: &IntentId, outcome: &Settled) -> Result<()> {
        self.note("settle");
        if let Some(why) = &self.settle_fails {
            return Err(Error::could_not_run(why.clone()));
        }
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let one = held
            .iter_mut()
            .find(|held| &held.intent.id == id)
            .ok_or_else(|| Error::usage(format!("no intent {id}")))?;
        one.settled = Some(outcome.clone());
        Ok(())
    }

    fn observe(&self, id: &IntentId, found: Option<u64>) -> Result<()> {
        let _ = (id, found);
        Ok(())
    }

    fn open(&self) -> Result<Vec<Intent>> {
        let held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(held
            .iter()
            .filter(|held| view(held).open)
            .map(|held| held.intent.clone())
            .collect())
    }

    fn latest(&self, op: WriteOp, key: &str) -> Result<Option<IntentView>> {
        let held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(held
            .iter()
            .rev()
            .find(|held| held.intent.op == op && held.intent.key == key)
            .map(view))
    }
}

#[test]
fn pacing_spaces_write_attempts_by_one_second() {
    let root = scratch();
    let fake = fake();
    let gh = github(canonical(root.path()), &fake);
    let permit = permit_for(&gh, &app_write());
    for _ in 0..3 {
        fake.fault(patches(), answer(200, "{}"));
    }
    gh.call_write(&patch(), &permit, &How::write())
        .expect("the first write");
    gh.call_write(&patch(), &permit, &How::write())
        .expect("the second write");
    assert_eq!(gh.fx().slept(), vec![Duration::from_secs(1)]);
    gh.fx().sleep(Duration::from_secs(5)).expect("the clock");
    gh.call_write(&patch(), &permit, &How::write())
        .expect("the third write");
    assert_eq!(
        gh.fx().slept(),
        vec![Duration::from_secs(1), Duration::from_secs(5)]
    );
}

#[test]
fn a_one_shot_is_sent_once() {
    let root = scratch();
    let fake = fake();
    let gh = github(canonical(root.path()), &fake);
    let permit = permit_for(&gh, &app_write());
    fake.fault_always(posts(), answer(502, "{\"message\":\"Bad Gateway\"}"));
    let call = post_ref();
    let draft = gh
        .intent_draft(WriteOp::Api, &slug(APP), &call, &permit, "api POST x")
        .expect("the draft");
    let intents = MemIntents::default();
    let outcome = gh
        .one_shot(&call, &permit, &draft, &intents, &created_of)
        .expect("an outcome");
    let Outcome::Unknown { error, .. } = outcome else {
        panic!("a 502 on a POST is Unknown: {outcome:?}");
    };
    assert_eq!(error.side_effect, SideEffect::Unknown, "{error:?}");
    assert_eq!(methods(&fake), vec!["POST /repos/fixture-dev/app/git/refs"]);
}

#[test]
fn converging_retries_with_its_guard_before_each_attempt() {
    let root = scratch();
    let fake = fake();
    let gh = github(canonical(root.path()), &fake);
    let permit = permit_for(&gh, &app_write());
    fake.fault(patches(), answer(502, "{}"));
    fake.fault(patches(), answer(502, "{}"));
    fake.fault(patches(), answer(200, "{}"));
    let mut calls = Vec::new();
    let answered = gh
        .call_write_guarded(&patch(), &permit, &How::write(), &mut |attempt| {
            calls.push((attempt, fake.seen().len()));
            Ok(None)
        })
        .expect("the third attempt answers");
    assert_eq!(answered.status, 200);
    assert_eq!(calls, vec![(1, 0), (2, 1), (3, 2)]);
    assert_eq!(methods(&fake), vec!["PATCH /repos/fixture-dev/app"; 3]);
}

#[test]
fn a_guard_that_finds_the_state_ends_the_loop() {
    let root = scratch();
    let fake = fake();
    let gh = github(canonical(root.path()), &fake);
    let permit = permit_for(&gh, &app_write());
    fake.fault(patches(), answer(502, "{}"));
    fake.fault(patches(), answer(200, "{}"));
    let answered = gh
        .call_write_guarded(&patch(), &permit, &How::write(), &mut |attempt| {
            Ok((attempt > 1).then(|| answered(200, b"{\"found\":true}")))
        })
        .expect("the guard's answer");
    assert_eq!(answered.body, b"{\"found\":true}");
    assert_eq!(methods(&fake), vec!["PATCH /repos/fixture-dev/app"]);
}

/// An error after a retry says at least what the earlier attempts may have done: a 502 may
/// have landed, so a later 422, or a later guard read that fails, is not `none`; the control,
/// a first-attempt 422, is.
#[test]
fn an_error_after_a_retry_keeps_the_strongest_side_effect() {
    let root = scratch();
    let fake = fake();
    let gh = github(canonical(root.path()), &fake);
    let permit = permit_for(&gh, &app_write());
    fake.fault(
        patches(),
        answer(422, "{\"message\":\"Validation Failed\"}"),
    );
    let control = gh
        .call_write_guarded(&patch(), &permit, &How::write(), &mut |_| Ok(None))
        .expect_err("a 422");
    assert_eq!(control.side_effect, SideEffect::None, "{control:?}");
    fake.fault(patches(), answer(502, "{}"));
    fake.fault(
        patches(),
        answer(422, "{\"message\":\"Validation Failed\"}"),
    );
    let refused = gh
        .call_write_guarded(&patch(), &permit, &How::write(), &mut |_| Ok(None))
        .expect_err("the second attempt's 422");
    assert_eq!(refused.side_effect, SideEffect::Unknown, "{refused:?}");
    fake.fault(patches(), answer(502, "{}"));
    let guarded = gh
        .call_write_guarded(&patch(), &permit, &How::write(), &mut |attempt| {
            if attempt > 1 {
                return Err(Error::could_not_run("the guard's read failed"));
            }
            Ok(None)
        })
        .expect_err("the second guard fails");
    assert_eq!(guarded.side_effect, SideEffect::Unknown, "{guarded:?}");
}

/// A replay that diverges at the retry's sleep, after a 502 that may have landed, says that
/// the write may have landed: the sleep's error is made at least the earlier attempt's.
#[test]
fn a_replay_that_diverges_at_the_retrys_sleep_keeps_the_side_effect() {
    use crate::fx::tape::{TapeHeader, TapeMode};
    use crate::fx::{DOOR_SLEEP, read_tape};
    use astream_effects::EffectRecord;

    let root = scratch();
    let fake = fake();
    let tape = root.path().join("t.atape");
    let argv: Vec<String> = ["api", "-X", "PATCH", "repos/fixture-dev/app"]
        .map(String::from)
        .to_vec();
    let header = TapeHeader {
        mode: TapeMode::Credentialed,
        ..TapeHeader::for_run(&argv, &endpoints(&fake))
    };
    let policy = RetryPolicy {
        attempts: 2,
        pauses: [Duration::from_millis(1); 2],
        max_secondary_wait: Duration::ZERO,
    };
    let recording = Fx::record_with(&tape, &header, crate::redact::Redactor::new())
        .expect("a recording")
        .with_endpoints(&endpoints(&fake));
    let gh = builder(
        canonical(root.path()),
        Arc::new(HttpTransport::direct(Arc::new(MapEnv::new()))),
        endpoints(&fake),
    )
    .fx(Arc::new(recording))
    .retry(policy)
    .build();
    let permit = permit_for(&gh, &app_write());
    fake.fault(patches(), answer(502, "{}"));
    fake.fault(patches(), answer(200, "{}"));
    gh.call_write_guarded(&patch(), &permit, &How::write(), &mut |_| Ok(None))
        .expect("the retry answers");
    gh.fx().finish(0).expect("the tape is whole");

    let (header, mut records) = read_tape(&tape).expect("the tape reads");
    let sleep = records
        .iter()
        .position(|r| matches!(r, EffectRecord::Exchange { door, .. } if door == DOOR_SLEEP))
        .expect("the retry's sleep is taped");
    records.remove(sleep);
    let replay = builder(
        Router::empty(),
        Arc::new(ScriptedTransport::new()),
        endpoints(&fake),
    )
    .fx(Arc::new(Fx::replay_from(&header, records)))
    .retry(policy)
    .build();
    let permit = permit_for(&replay, &app_write());
    let error = replay
        .call_write_guarded(&patch(), &permit, &How::write(), &mut |_| Ok(None))
        .expect_err("the replay diverges at the sleep");
    assert!(matches!(error.sub, Sub::Replay(_)), "{error:?}");
    assert_eq!(error.side_effect, SideEffect::Unknown, "{error:?}");
}

#[test]
fn a_401_gets_one_attempt() {
    let root = scratch();
    let fake = fake();
    let gh = github(canonical(root.path()), &fake);
    let permit = permit_for(&gh, &app_write());
    fake.fault_always(patches(), answer(401, "{\"message\":\"Bad credentials\"}"));
    let error = gh
        .call_write(&patch(), &permit, &How::write())
        .expect_err("a 401");
    assert_eq!(error.kind, Kind::Unauthenticated, "{error:?}");
    assert_eq!(fake.seen().len(), 1);
}

#[test]
fn the_sleep_budget_ends_the_loop() {
    let root = scratch();
    let fake = fake();
    let gh = github(canonical(root.path()), &fake);
    let permit = permit_for(&gh, &app_write());
    fake.fault_always(patches(), Fault::SecondaryLimit { retry_after: 5 });
    let how = How::write().sleep_budget(Duration::from_secs(2));
    let error = gh
        .call_write(&patch(), &permit, &how)
        .expect_err("the budget ends it");
    assert_eq!(
        (error.kind, error.sub.clone()),
        (Kind::RateLimited, Sub::Secondary)
    );
    assert!(
        !gh.fx().slept().contains(&Duration::from_secs(5)),
        "{:?}",
        gh.fx().slept()
    );
    assert_eq!(fake.seen().len(), 1);
}

#[test]
fn the_guard_reads_skip_the_rate_book() {
    let root = scratch();
    let fake = fake();
    let dirs = Dirs::under(root.path());
    let book = RateBook::at(dirs.rate());
    let key = identity::fingerprint(agh_fake::token("release").as_bytes());
    book.record(
        &key,
        &RateSnapshot {
            resource: "core".to_string(),
            limit: 5000,
            remaining: 0,
            reset: SIM_START + 3600,
            used: 5000,
        },
    );
    let gh = builder(
        canonical(root.path()),
        Arc::new(HttpTransport::direct(Arc::new(MapEnv::new()))),
        endpoints(&fake),
    )
    .rate_book(book)
    .build();
    let permit = permit_for(&gh, &aterm_write());
    let write = Call::write(
        Method::Patch,
        "/repos/alabsystems/aterm",
        aterm_write(),
        Idempotency::Converging,
        CallBody::Json(b"{}".to_vec()),
    );
    let error = gh
        .call_write(&write, &permit, &How::write())
        .expect_err("the book refuses the write");
    assert_eq!(
        (error.kind, error.sub.clone()),
        (Kind::RateLimited, Sub::PreRefused)
    );
    let reader = How::writer().named(aterm_literal());
    let target = || RouteTarget::Repo(slug(ATERM));
    let booked = gh
        .call(&reader.get("/repos/alabsystems/aterm", target()))
        .expect_err("a booked read is refused too");
    assert_eq!(booked.sub, Sub::PreRefused, "{booked:?}");
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
    gh.call(
        &reader
            .rate_book(false)
            .get("/repos/alabsystems/aterm", target()),
    )
    .expect("the guard's read is sent");
    assert_eq!(methods(&fake), vec!["GET /repos/alabsystems/aterm"]);
}

/// A client of one inline profile `bot`, bound to `fixture-dev-bot`, whose token the fake
/// knows as `login`.
fn inline_client(fake: &FakeGithub, login: &str) -> Github {
    let token = agh_fake::token("inline");
    fake.add_token(&token, login);
    let inline =
        InlineToken::new(aterm_uds::secret::Secret::new(token.as_bytes())).expect("a token");
    let profile =
        Profile::inline("bot", Tier::Dev, "fixture-dev-bot", inline).expect("an inline profile");
    github(Router::single_identity(profile), fake)
}

fn add_key() -> Call {
    Call::write(
        Method::Post,
        "/user/keys",
        RouteRequest::write(RouteTarget::Identity),
        Idempotency::OneShot,
        CallBody::Json(b"{\"title\":\"m\",\"key\":\"ssh-ed25519 AAAA\"}".to_vec()),
    )
}

#[test]
fn an_inline_profile_binds_its_login_first() {
    let fake = fake();
    let gh = inline_client(&fake, "fixture-dev-bot");
    let permit = permit_for(&gh, &RouteRequest::write(RouteTarget::Identity));
    fake.fault(
        Match::api("/user/keys").method("POST"),
        answer(201, "{\"id\":1}"),
    );
    gh.call_write(&add_key(), &permit, &How::write())
        .expect("the key write");
    assert_eq!(methods(&fake), vec!["GET /user", "POST /user/keys"]);

    let other = FakeGithub::start();
    let gh = inline_client(&other, "someone-else");
    let permit = permit_for(&gh, &RouteRequest::write(RouteTarget::Identity));
    let error = gh
        .call_write(&add_key(), &permit, &How::write())
        .expect_err("the login is another's");
    assert_eq!(error.kind, Kind::Unauthenticated, "{error:?}");
    assert_eq!(
        error.sub.slug().as_deref(),
        Some("login_mismatch"),
        "{error:?}"
    );
    assert_eq!(methods(&other), vec!["GET /user"]);
}

/// The hang detector of a wait for something that must happen (plan §1), never a budget.
const HANG: Duration = Duration::from_secs(60);

/// Wait for `done`, failing past the hang detector.
fn until(what: &str, done: &dyn Fn() -> bool) {
    let deadline = std::time::Instant::now() + HANG;
    while !done() {
        assert!(
            std::time::Instant::now() < deadline,
            "hung waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_send_waits_for_another_send_s_binding() {
    let fake = FakeGithub::start();
    let gh = inline_client(&fake, "someone-else");
    fake.fault(Match::api("/user").method("GET"), Fault::Hang(HANG));
    let (done, finished) = std::sync::mpsc::channel();
    let send = |name: &'static str| {
        let gh = gh.clone();
        let done = done.clone();
        std::thread::spawn(move || {
            let permit = permit_for(&gh, &RouteRequest::write(RouteTarget::Identity));
            let _ = done.send((name, gh.call_write(&add_key(), &permit, &How::write())));
        })
    };
    let first = send("first");
    until("the first binding's GET /user", &|| !fake.seen().is_empty());
    let second = send("second");
    until("the second send to wait", &|| gh.bindings().waiting == 1);
    fake.end_hangs();
    for _ in 0..2 {
        let (name, result) = finished.recv_timeout(HANG).expect("a send ended");
        let error = result.expect_err("the login is another's");
        if name == "second" {
            assert_eq!(
                error.sub.slug().as_deref(),
                Some("login_mismatch"),
                "{error:?}"
            );
        }
    }
    first.join().expect("the first send");
    second.join().expect("the second send");
    let methods = methods(&fake);
    assert!(
        methods.iter().all(|method| method == "GET /user"),
        "{methods:?}"
    );
}

fn http(status: u16) -> HttpRef {
    HttpRef {
        status,
        method: "POST",
        path: "/repos/fixture-dev/app/git/refs".to_string(),
        request_id: None,
    }
}

#[test]
fn the_settle_table_row_by_row() {
    let not_sent = Error::could_not_run("connect: refused").with_sub(Sub::Wire(WireKind::Connect));
    assert_eq!(settle_table(&Err(not_sent), None), Settled::NeverSent);
    let url = "http://127.0.0.1/repos/fixture-dev/app/git/refs/tags/v1".to_string();
    assert_eq!(
        settle_table(&Ok(answered(201, b"{}")), Some((5, url.clone()))),
        Settled::Created { id: 5, url }
    );
    assert!(matches!(
        settle_table(&Ok(answered(201, b"{}")), None),
        Settled::Unknown { .. }
    ));
    let refused = Error::new(Kind::Invalid, "GitHub answered 422").with_http(http(422));
    assert_eq!(
        settle_table(&Err(refused), None),
        Settled::Refused { status: 422 }
    );
    let bad_gateway = Error::new(Kind::Server, "GitHub answered 502")
        .with_side_effect(SideEffect::Unknown)
        .with_http(http(502));
    assert!(matches!(
        settle_table(&Err(bad_gateway), None),
        Settled::Unknown { .. }
    ));
}

#[test]
fn a_502_whose_why_holds_a_token_settles_unknown_without_it() {
    let root = scratch();
    let fake = fake();
    let gh = github(canonical(root.path()), &fake);
    let permit = permit_for(&gh, &app_write());
    let token = agh_fake::token("7");
    fake.fault_always(
        posts(),
        answer(502, &format!("{{\"message\":\"{token}\"}}")),
    );
    let call = post_ref();
    let draft = gh
        .intent_draft(WriteOp::Api, &slug(APP), &call, &permit, "api POST x")
        .expect("the draft");
    let intents = MemIntents::default();
    gh.one_shot(&call, &permit, &draft, &intents, &created_of)
        .expect("an outcome");
    let settled = intents.settled();
    let [Some(Settled::Unknown { why })] = settled.as_slice() else {
        panic!("one Unknown settle: {settled:?}");
    };
    assert!(!why.contains(&token), "the journal's why holds the token");
}

/// A recorded one-shot whose answer the tape refuses (a loopback fake that echoes a token
/// shape) settles `Unknown` (§7, §4.6.5 item 8): the credentialed recording (§14) sent the
/// write, so GitHub may have applied it, and the tape could not hold what it answered.
#[test]
fn a_recorded_one_shot_whose_answer_the_tape_refuses_settles_unknown() {
    use crate::fx::tape::{TapeHeader, TapeMode};

    let root = scratch();
    let fake = fake();
    let endpoints = endpoints(&fake);
    let mut header = TapeHeader::for_run(&["api".to_string()], &endpoints);
    header.mode = TapeMode::Credentialed;
    let tape = root.path().join("write.atape");
    let fx = Fx::record_for_tests(&tape, &header).expect("a recording");
    let gh = builder(
        canonical(root.path()),
        Arc::new(HttpTransport::direct(Arc::new(MapEnv::new()))),
        endpoints,
    )
    .fx(Arc::new(fx))
    .build();
    let permit = permit_for(&gh, &app_write());
    let token = agh_fake::token("echoed");
    fake.fault_always(
        posts(),
        answer(
            201,
            &format!("{{\"id\":5,\"url\":\"x\",\"note\":\"{token}\"}}"),
        ),
    );
    let call = post_ref();
    let draft = gh
        .intent_draft(WriteOp::Api, &slug(APP), &call, &permit, "api POST x")
        .expect("the draft");
    let intents = MemIntents::default();
    let outcome = gh
        .one_shot(&call, &permit, &draft, &intents, &created_of)
        .expect("an outcome");
    let Outcome::Unknown { error, .. } = &outcome else {
        panic!("a sent write whose answer the tape refuses is unknown: {outcome:?}");
    };
    assert_eq!(
        error.sub.slug().as_deref(),
        Some("replay.body_not_taped"),
        "{error:?}"
    );
    let settled = intents.settled();
    let [Some(Settled::Unknown { why })] = settled.as_slice() else {
        panic!("one Unknown settle: {settled:?}");
    };
    assert!(!why.contains(&token), "the journal's why holds the token");
    assert_eq!(
        methods(&fake)
            .iter()
            .filter(|m| m.starts_with("POST"))
            .count(),
        1,
        "{:?}",
        methods(&fake)
    );
}

#[test]
fn one_shot_begins_before_it_sends() {
    let root = scratch();
    let transport = Arc::new(ScriptedTransport::new());
    let gh = builder(
        canonical(root.path()),
        transport.clone(),
        Endpoints::loopback(9000, 9001, 9002, 9003),
    )
    .build();
    let permit = permit_for(&gh, &app_write());
    transport.answer(
        201,
        &[("content-type", "application/json")],
        b"{\"id\":5,\"url\":\"http://127.0.0.1:9000/x\"}",
    );
    let call = post_ref();
    let draft = gh
        .intent_draft(WriteOp::Api, &slug(APP), &call, &permit, "api POST x")
        .expect("the draft");
    let intents = MemIntents::watching(&transport);
    let outcome = gh
        .one_shot(&call, &permit, &draft, &intents, &created_of)
        .expect("an outcome");
    assert!(outcome.done().is_some(), "the POST is done");
    assert_eq!(intents.log(), vec!["begin", "POST", "settle"]);
}

/// A settle that fails after GitHub answered 201 is could_not_run with side effect applied
/// (the write landed), next `agh intent show {repo} {id}`, and its why holds no token, even
/// one the journal's own error carried.
#[test]
fn a_settle_that_fails_after_a_2xx_is_applied_and_names_the_intent() {
    let root = scratch();
    let transport = Arc::new(ScriptedTransport::new());
    let gh = builder(
        canonical(root.path()),
        transport.clone(),
        Endpoints::loopback(9000, 9001, 9002, 9003),
    )
    .build();
    let permit = permit_for(&gh, &app_write());
    transport.answer(
        201,
        &[("content-type", "application/json")],
        b"{\"id\":5,\"url\":\"http://127.0.0.1:9000/x\"}",
    );
    let call = post_ref();
    let draft = gh
        .intent_draft(WriteOp::Api, &slug(APP), &call, &permit, "api POST x")
        .expect("the draft");
    let token = agh_fake::token("settle");
    let intents = MemIntents {
        settle_fails: Some(format!("the disk is full near {token}")),
        ..MemIntents::watching(&transport)
    };
    let error = gh
        .one_shot(&call, &permit, &draft, &intents, &created_of)
        .expect_err("the settle failed");
    assert_eq!(
        (error.kind, error.side_effect, &error.sub),
        (Kind::CouldNotRun, SideEffect::Applied, &Sub::Local),
        "{error:?}"
    );
    let id = "0-0123456789ab";
    assert_eq!(
        error.next,
        format!("agh intent show {APP} {id}"),
        "{error:?}"
    );
    assert!(error.why.contains(id), "{error:?}");
    assert!(!error.why.contains(&token), "the why holds the token");
    assert_eq!(intents.log(), vec!["begin", "POST", "settle"]);
    assert_eq!(intents.settled(), vec![None], "the intent stays open");
}

#[test]
fn intent_draft_fills_the_request_digest() {
    let root = scratch();
    let fake = fake();
    let gh = github(canonical(root.path()), &fake);
    let permit = permit_for(&gh, &app_write());
    let call = post_ref();
    let draft = gh
        .intent_draft(WriteOp::Api, &slug(APP), &call, &permit, "api POST x")
        .expect("the draft");
    assert_eq!(
        draft.fingerprint,
        identity::fingerprint(agh_fake::token("dev").as_bytes())
    );
    let decided = gh.decided_for(&permit).expect("the decision");
    let prepared = gh.prepare_write(&call, &decided);
    let CallBody::Json(body) = &call.body else {
        panic!("a JSON body");
    };
    let request = prepared.write_request(&permit, WireBody::Bytes(body), None);
    let canonical =
        canon::request_credentialed(&request, gh.endpoints()).expect("the canonical bytes");
    assert_eq!(draft.request, canon::request_digest(&canonical));
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
}

#[test]
fn a_call_for_another_target_than_its_permit_is_usage() {
    let root = scratch();
    let fake = fake();
    let gh = github(canonical(root.path()), &fake);
    let permit = permit_for(&gh, &app_write());
    let call = Call::write(
        Method::Patch,
        "/repos/fixture-dev/other",
        RouteRequest::write(RouteTarget::Repo(slug("fixture-dev/other"))),
        Idempotency::Converging,
        CallBody::Json(b"{}".to_vec()),
    );
    let error = gh
        .call_write(&call, &permit, &How::write())
        .expect_err("another target");
    assert_eq!(error.kind, Kind::Usage, "{error:?}");
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
}

#[test]
fn a_changed_upload_file_is_refused_before_a_byte_leaves() {
    let root = scratch();
    let fake = fake();
    let gh = github(canonical(root.path()), &fake);
    let permit = permit_for(&gh, &app_write());
    let path = root.path().join("asset.bin");
    std::fs::write(&path, b"twelve bytes").expect("the asset");
    let call = Call::write(
        Method::Post,
        "/repos/fixture-dev/app/releases/1/assets",
        app_write(),
        Idempotency::OneShot,
        CallBody::File {
            path: path.clone(),
            len: 12,
            content_type: "application/octet-stream".to_string(),
        },
    )
    .origin(OriginRole::Uploads)
    .query("name", "asset.bin");
    std::fs::write(&path, b"thirteen bytes").expect("the change");
    let error = gh
        .call_write(&call, &permit, &How::write())
        .expect_err("the file changed");
    assert_eq!(error.kind, Kind::CouldNotRun, "{error:?}");
    assert!(
        error.why.contains("changed since it was hashed"),
        "{error:?}"
    );
    assert_eq!(error.side_effect, SideEffect::None, "{error:?}");
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
}

#[test]
fn a_failed_binding_read_is_a_write_never_sent() {
    let fake = FakeGithub::start();
    let gh = inline_client(&fake, "fixture-dev-bot");
    fake.fault_always(
        Match::api("/user").method("GET"),
        answer(502, "{\"message\":\"Bad Gateway\"}"),
    );
    let permit = permit_for(&gh, &RouteRequest::write(RouteTarget::Identity));
    let result = gh.call_write(&add_key(), &permit, &How::write());
    let error = result.as_ref().expect_err("the binding read failed");
    assert_eq!(error.side_effect, SideEffect::None, "{error:?}");
    assert_eq!(settle_table(&result, None), Settled::NeverSent);

    // Through `one_shot`, the binding fails before `begin`: no intent is left.
    let call = add_key();
    let draft = gh
        .intent_draft(
            WriteOp::Api,
            &slug(APP),
            &call,
            &permit,
            "api POST /user/keys",
        )
        .expect("the draft");
    let intents = MemIntents::default();
    gh.one_shot(&call, &permit, &draft, &intents, &created_of)
        .expect_err("the binding read failed");
    assert!(intents.settled().is_empty(), "{:?}", intents.settled());
    let methods = methods(&fake);
    assert!(
        methods.iter().all(|method| method == "GET /user"),
        "{methods:?}"
    );
}

#[test]
fn a_one_shot_outside_its_permit_begins_nothing() {
    let root = scratch();
    let fake = fake();
    let gh = github(canonical(root.path()), &fake);
    let permit = permit_for(&gh, &app_write());
    let call = Call::write(
        Method::Post,
        "/repos/fixture-dev/other/git/refs",
        RouteRequest::write(RouteTarget::Repo(slug("fixture-dev/other"))),
        Idempotency::OneShot,
        CallBody::Json(b"{}".to_vec()),
    );
    let draft = gh
        .intent_draft(WriteOp::Api, &slug(APP), &post_ref(), &permit, "api POST x")
        .expect("the draft");
    let intents = MemIntents::default();
    let error = gh
        .one_shot(&call, &permit, &draft, &intents, &created_of)
        .expect_err("another target");
    assert_eq!(error.kind, Kind::Usage, "{error:?}");
    assert!(intents.settled().is_empty(), "{:?}", intents.settled());
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
}

#[test]
fn a_created_url_is_redacted_before_the_journal() {
    let root = scratch();
    let transport = Arc::new(ScriptedTransport::new());
    let gh = builder(
        canonical(root.path()),
        transport.clone(),
        Endpoints::loopback(9000, 9001, 9002, 9003),
    )
    .build();
    let learned = "learned-secret-of-no-shape";
    gh.redactor().learn(learned.as_bytes());
    let permit = permit_for(&gh, &app_write());
    transport.answer(
        201,
        &[("content-type", "application/json")],
        format!("{{\"id\":5,\"url\":\"http://127.0.0.1:9000/{learned}\"}}").as_bytes(),
    );
    let call = post_ref();
    let draft = gh
        .intent_draft(WriteOp::Api, &slug(APP), &call, &permit, "api POST x")
        .expect("the draft");
    let intents = MemIntents::default();
    gh.one_shot(&call, &permit, &draft, &intents, &created_of)
        .expect("an outcome");
    let settled = intents.settled();
    let [Some(Settled::Created { id: 5, url })] = settled.as_slice() else {
        panic!("one Created settle: {settled:?}");
    };
    assert!(!url.contains(learned), "the journal's url holds the secret");
}
