// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A native ref write behind another writer's open intent, and a refresh of origin/main
//! beside another refresh: each waits or reads again within its bound, on a real journal and
//! a real local bare repository, and refuses past it.

use super::super::write::{INTENT_POLL, INTENT_WAIT, batch_request, control_request};
use super::*;
use agh::github::refs::{AtomicRefBatch, RefUpdate};
use agh::journal::{IntentDraft, IntentStore, JournalRecord, Settled};
use agh::policy::{RouteRequest, RouteTarget};
use agh::write::WriteOp;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const MAIN: &str = "refs/heads/main";

/// A bare origin whose main is `old`, and a source checkout that also holds `new`, its child.
struct World {
    _root: aterm_tempfile::TempDir,
    bare: PathBuf,
    source: PathBuf,
    old: String,
    new: String,
}

fn world() -> World {
    let root = aterm_tempfile::tempdir().unwrap();
    let bare = repo(root.path(), "remote.git", true);
    let source = repo(root.path(), "source", false);
    let old = commit(&source, "remote main\n");
    git(&source, &["push", "-q", bare.to_str().unwrap(), "main"]);
    let new = commit(&source, "next main\n");
    git(
        &source,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    World {
        _root: root,
        bare,
        source,
        old,
        new,
    }
}

impl World {
    fn remote(&self) -> (Remote, Arc<agh::Fx>) {
        let fx = Arc::new(agh::Fx::simulated(1_760_000_000));
        (
            claim_fixture_with_fx(&self.source, &self.bare, fx.clone()),
            fx,
        )
    }

    fn main(&self) -> String {
        git(&self.bare, &["rev-parse", MAIN])
    }

    /// Origin takes `new` by a push this journal holds no intent of.
    fn land_new(&self) {
        let spec = format!("{}:{MAIN}", self.new);
        git(
            &self.source,
            &["push", "-q", self.bare.to_str().unwrap(), &spec],
        );
    }

    /// What the journal records of this world's write of main, `old` to `new`.
    fn request(&self, remote: &Remote) -> String {
        let batch = AtomicRefBatch::new(vec![RefUpdate {
            reference: MAIN.into(),
            before: self.old.clone(),
            after: self.new.clone(),
        }])
        .unwrap();
        batch_request(&remote.slug, &remote.repo, &batch)
    }
}

/// The control transaction that takes the release lock's lease for `new`, where origin has
/// none.
fn lease(w: &World) -> crate::ledger::ControlRefTransaction {
    crate::ledger::ControlRefTransaction::new(
        &w.new,
        vec![crate::ledger::ControlRefChange {
            reference: agh::git::RELEASE_LEASE_REF.into(),
            before: None,
            after: Some(w.new.clone()),
        }],
    )
    .unwrap()
}

/// What origin holds at the release lock's lease: empty where it has none.
fn held_lease(w: &World) -> String {
    git(
        &w.bare,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            agh::git::RELEASE_LEASE_REF,
        ],
    )
}

/// Begin an intent in the repository's atomic-ref bucket as another agh write would, and
/// leave it in flight.
fn begin(remote: &Remote, request: &str) -> agh::IntentId {
    let route = RouteRequest::write(RouteTarget::Repo(remote.slug.clone()));
    let permit = remote.client.permit(&route).unwrap();
    let journal = remote.client.journal(&remote.slug).unwrap();
    let latest = journal
        .latest(WriteOp::RefUpdateAtomic, "atomic refs")
        .unwrap()
        .map(|view| view.intent.id);
    let draft = IntentDraft::new(
        WriteOp::RefUpdateAtomic,
        &remote.slug,
        &permit,
        "fixture-fingerprint",
        "atomic refs",
    )
    .with_request(request.into())
    .with_after(latest);
    journal.begin(&draft).unwrap()
}

fn settle(remote: &Remote, intent: &agh::IntentId, outcome: &Settled) {
    remote
        .client
        .journal(&remote.slug)
        .unwrap()
        .settle(intent, outcome)
        .unwrap();
}

fn created() -> Settled {
    Settled::Created {
        id: 0,
        url: String::new(),
    }
}

fn unknown() -> Settled {
    Settled::Unknown {
        why: "fixture lost answer".into(),
    }
}

/// How many intents the journal holds: one more for every write that began, and so for every
/// write that could have been sent.
fn intents(remote: &Remote) -> usize {
    let (records, _) = remote.client.journal(&remote.slug).unwrap().read().unwrap();
    records
        .iter()
        .filter(|(_, record)| matches!(record, JournalRecord::Intent(_)))
        .count()
}

fn view(remote: &Remote, intent: &agh::IntentId) -> agh::journal::IntentView {
    remote
        .client
        .journal(&remote.slug)
        .unwrap()
        .view(intent)
        .unwrap()
        .unwrap()
}

/// Keep every line `remote` says, and run `on_wait` when it says it waits: the other writer's
/// part, at the one moment the wait hands it.
fn said(
    remote: &mut Remote,
    on_wait: impl Fn() + Send + Sync + 'static,
) -> Arc<Mutex<Vec<String>>> {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&lines);
    remote.notice = Box::new(move |line| {
        kept.lock().unwrap().push(line.to_owned());
        if line.starts_with("waiting for intent ") {
            assert!(
                crate::github_remote::intent_wait().is_some(),
                "the lease heartbeat reads a wait while it lasts"
            );
            on_wait();
        }
    });
    lines
}

fn lines(said: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    said.lock().unwrap().clone()
}

/// R31(b): another writer's write in flight (its intent open in the repository's one bucket)
/// refused the cut's write, which had sent nothing, and the run ended. The write now waits
/// for that intent to close, says one line, and is then made: a batch and a control
/// transaction alike.
#[test]
fn a_write_waits_for_another_writers_open_intent_and_then_writes() {
    let w = world();
    let (mut remote, fx) = w.remote();
    let foreign = begin(&remote, "sha256:another-writers-push 162");
    let closing = (
        remote.client.journal(&remote.slug).unwrap(),
        foreign.clone(),
    );
    let said_lines = said(&mut remote, move || {
        closing.0.settle(&closing.1, &created()).unwrap();
    });
    assert_eq!(
        remote.write_ref(MAIN, Some(&w.old), &w.new).unwrap(),
        RefWrite::Written
    );
    assert_eq!(w.main(), w.new);
    let written = remote
        .client
        .journal(&remote.slug)
        .unwrap()
        .latest(WriteOp::RefUpdateAtomic, "atomic refs")
        .unwrap()
        .unwrap();
    assert_eq!(
        written.intent.request,
        w.request(&remote),
        "the request this path derives for a batch is the one agh's journal holds of its push"
    );
    assert_eq!(
        lines(&said_lines),
        [format!(
            "waiting for intent {foreign} of another writer on this Mac"
        )]
    );
    assert_eq!(fx.slept(), [INTENT_POLL], "one look found it closed");
    assert_eq!(intents(&remote), 2, "the other writer's and this write's");

    let foreign = begin(&remote, "sha256:another-writers-claim 178");
    let closing = (
        remote.client.journal(&remote.slug).unwrap(),
        foreign.clone(),
    );
    let said_lines = said(&mut remote, move || {
        closing.0.settle(&closing.1, &Settled::NeverSent).unwrap();
    });
    assert_eq!(
        crate::ledger::Origin::control_transaction(&remote, lease(&w)).unwrap(),
        RefWrite::Written
    );
    assert_eq!(held_lease(&w), w.new);
    let written = remote
        .client
        .journal(&remote.slug)
        .unwrap()
        .latest(WriteOp::RefUpdateAtomic, "atomic refs")
        .unwrap()
        .unwrap();
    assert_eq!(
        written.intent.request,
        control_request(&remote.slug, &remote.repo, &lease(&w)),
        "the request this path derives for a control transaction is the one agh's journal \
         holds of its push"
    );
    assert_eq!(
        lines(&said_lines),
        [format!(
            "waiting for intent {foreign} of another writer on this Mac"
        )]
    );
}

/// Negative control: an intent that stays open past the bound refuses the write as before,
/// naming the intent and `agh intent show`, and nothing was sent. A stopped wait (the lease
/// worker's monitor ending) refuses at once.
#[test]
fn a_wait_past_its_bound_refuses_naming_the_intent_and_sends_nothing() {
    let w = world();
    let (mut remote, fx) = w.remote();
    let foreign = begin(&remote, "sha256:a-writer-that-died 162");
    let said_lines = said(&mut remote, || {});
    let error = remote.write_ref(MAIN, Some(&w.old), &w.new).unwrap_err();
    for words in [
        format!("intent {foreign} for atomic refs began first: agh never posts twice"),
        format!("agh intent show fixture/app {foreign}"),
        format!(
            "waited {} s for intent {foreign} to close and it is still open",
            INTENT_WAIT.as_secs()
        ),
    ] {
        assert!(error.contains(&words), "{error}");
    }
    assert_eq!(w.main(), w.old);
    assert_eq!(intents(&remote), 1, "this write never began");
    assert_eq!(fx.slept().iter().sum::<Duration>(), INTENT_WAIT);
    assert_eq!(lines(&said_lines).len(), 1, "one line for the whole wait");

    let looks = fx.slept().len();
    let stopped = remote
        .write_claim_ref(MAIN, Some(&w.old), &w.new, None, &|| true)
        .err()
        .unwrap();
    assert!(
        stopped.contains(&format!(
            "stopped while waiting for intent {foreign} to close"
        )),
        "{stopped}"
    );
    assert_eq!(fx.slept().len(), looks);
    assert_eq!(w.main(), w.old);
}

/// Negative control: a refusal that names no open intent is returned as it came, with no wait
/// and no line.
#[test]
fn a_refusal_that_names_no_open_intent_is_returned_as_it_came() {
    let w = world();
    let (mut remote, fx) = w.remote();
    let said_lines = said(&mut remote, || {});
    // The expected old object is not origin's: the push's own lease refuses.
    remote.fetch_exact(&w.old).unwrap();
    let other = git(
        &w.source,
        &["commit-tree", &format!("{}^{{tree}}", w.old), "-m", "other"],
    );
    assert_eq!(
        remote.write_ref(MAIN, Some(&other), &w.new).unwrap(),
        RefWrite::Conflict
    );
    assert_eq!(w.main(), w.old);
    assert!(fx.slept().is_empty(), "nothing waited");
    assert!(lines(&said_lines).is_empty(), "no line was said");
}

/// An open intent that is an earlier attempt at this same write (the journal's `request` is
/// this write's) is settled by reading origin, as agh settles an uncertain push: origin
/// holding every ref the write asked for establishes it, the intent is recorded found, and
/// nothing is sent again.
#[test]
fn an_earlier_attempt_at_the_same_write_is_settled_by_reading_origin() {
    // Left open with an unknown outcome, and origin took it.
    let w = world();
    let (mut remote, fx) = w.remote();
    let own = begin(&remote, &w.request(&remote));
    settle(&remote, &own, &unknown());
    w.land_new();
    let said_lines = said(&mut remote, || {});
    assert_eq!(
        remote.write_ref(MAIN, Some(&w.old), &w.new).unwrap(),
        RefWrite::Written
    );
    assert_eq!(
        lines(&said_lines),
        [format!(
            "intent {own} is an earlier attempt at this write that was left open: origin holds \
             every ref it asked for, and nothing is sent again"
        )]
    );
    let seen = view(&remote, &own);
    assert!(!seen.open);
    assert_eq!(
        seen.observed
            .iter()
            .map(|(found, _)| *found)
            .collect::<Vec<_>>(),
        [Some(0)]
    );
    assert_eq!(intents(&remote), 1, "no second write began");
    assert!(fx.slept().is_empty(), "nothing waited");

    // Still in flight in another process, which finishes it while this one waits.
    let w = world();
    let (mut remote, _) = w.remote();
    let own = begin(&remote, &w.request(&remote));
    let finishing = (
        remote.client.journal(&remote.slug).unwrap(),
        own.clone(),
        w.source.clone(),
        w.bare.clone(),
        format!("{}:{MAIN}", w.new),
    );
    let said_lines = said(&mut remote, move || {
        let (journal, own, source, bare, spec) = &finishing;
        git(source, &["push", "-q", bare.to_str().unwrap(), spec]);
        journal.settle(own, &created()).unwrap();
    });
    assert_eq!(
        remote.write_ref(MAIN, Some(&w.old), &w.new).unwrap(),
        RefWrite::Written
    );
    assert_eq!(
        lines(&said_lines),
        [
            format!("waiting for intent {own} of an earlier attempt at this write"),
            format!(
                "intent {own} is an earlier attempt at this write that its writer finished: \
                 origin holds every ref it asked for, and nothing is sent again"
            )
        ]
    );
    assert_eq!(intents(&remote), 1, "no second write began");
}

/// Negative control: an earlier attempt whose outcome is unknown, with origin still at the
/// old object, is not posted again on the strength of that read: its intent stays open, and
/// past the bound the write refuses.
#[test]
fn an_earlier_attempt_origin_does_not_hold_is_never_posted_again() {
    let w = world();
    let (mut remote, _) = w.remote();
    let own = begin(&remote, &w.request(&remote));
    settle(&remote, &own, &unknown());
    let said_lines = said(&mut remote, || {});
    let error = remote.write_ref(MAIN, Some(&w.old), &w.new).unwrap_err();
    assert!(
        error.contains(&format!("agh intent show fixture/app {own}")),
        "{error}"
    );
    assert!(error.contains("it is still open"), "{error}");
    assert_eq!(w.main(), w.old, "a second post would have moved main");
    assert!(view(&remote, &own).open);
    assert_eq!(intents(&remote), 1, "no second write began");
    assert_eq!(
        lines(&said_lines),
        [format!(
            "waiting for intent {own} of an earlier attempt at this write"
        )]
    );
}

/// A write whose own answer is lost (the push ran, and its acknowledgement did not come back)
/// is settled before the call returns: origin holds what it asked for, so its intent is
/// recorded found and the write is made. Where origin does not hold it, the outcome stays
/// unknown and the intent open, as before.
/// A control transaction (the release lock, a publication's control refs) is matched to its
/// earlier attempt as a batch is: left open with origin holding what it asked, it is recorded
/// found and nothing is sent again. Control: where origin does not hold it, the intent stays
/// open, the write refuses naming it, and origin is not written.
#[test]
fn an_earlier_attempt_at_the_same_control_transaction_is_settled_by_reading_origin() {
    let w = world();
    let (mut remote, fx) = w.remote();
    let request = control_request(&remote.slug, &remote.repo, &lease(&w));
    let own = begin(&remote, &request);
    settle(&remote, &own, &unknown());
    let spec = format!("{}:{}", w.new, agh::git::RELEASE_LEASE_REF);
    git(&w.source, &["push", "-q", w.bare.to_str().unwrap(), &spec]);
    let said_lines = said(&mut remote, || {});
    assert_eq!(
        crate::ledger::Origin::control_transaction(&remote, lease(&w)).unwrap(),
        RefWrite::Written
    );
    assert_eq!(
        lines(&said_lines),
        [format!(
            "intent {own} is an earlier attempt at this write that was left open: origin holds \
             every ref it asked for, and nothing is sent again"
        )]
    );
    assert!(!view(&remote, &own).open);
    assert_eq!(intents(&remote), 1, "no second write began");
    assert!(fx.slept().is_empty(), "nothing waited");

    let w = world();
    let (mut remote, _) = w.remote();
    let request = control_request(&remote.slug, &remote.repo, &lease(&w));
    let own = begin(&remote, &request);
    settle(&remote, &own, &unknown());
    let said_lines = said(&mut remote, || {});
    let error = crate::ledger::Origin::control_transaction(&remote, lease(&w)).unwrap_err();
    assert!(
        error.contains(&format!("agh intent show fixture/app {own}")),
        "{error}"
    );
    assert!(error.contains("it is still open"), "{error}");
    assert_eq!(
        held_lease(&w),
        "",
        "a second post would have taken the lease"
    );
    assert!(view(&remote, &own).open);
    assert_eq!(intents(&remote), 1, "no second write began");
    assert_eq!(
        lines(&said_lines),
        [format!(
            "waiting for intent {own} of an earlier attempt at this write"
        )]
    );
}

#[test]
fn a_writes_own_lost_answer_is_settled_by_reading_origin() {
    for held in [true, false] {
        let w = world();
        let probe = claim_fixture(&w.source, &w.bare);
        let batch = AtomicRefBatch::new(vec![RefUpdate {
            reference: MAIN.into(),
            before: w.old.clone(),
            after: w.new.clone(),
        }])
        .unwrap();
        let route = RouteRequest::write(RouteTarget::Repo(probe.slug.clone()));
        let permit = probe.client.permit(&route).unwrap();
        probe
            .client
            .refs(&probe.slug)
            .check_push(&permit, &probe.repo, &batch)
            .unwrap();
        let push = probe
            .client
            .fx()
            .children_run()
            .into_iter()
            .find(|child| child.args.iter().any(|arg| arg == "push"))
            .unwrap();
        drop(probe);
        let (bare, old) = (w.bare.clone(), w.old.clone());
        let fx = agh::Fx::simulated(1_760_000_000).with_git_intercept_once_for_tests(
            push.program,
            push.args
                .into_iter()
                .filter(|arg| arg != "--dry-run")
                .collect(),
            || {},
            move |result| {
                // The push ran; its per-ref acknowledgement is lost.
                let out = result.as_mut().unwrap();
                assert_eq!(out.code, Some(0));
                out.stdout.clear();
                if !held {
                    git(&bare, &["update-ref", MAIN, &old]);
                }
            },
        );
        let mut remote = claim_fixture_with_fx(&w.source, &w.bare, Arc::new(fx));
        let said_lines = said(&mut remote, || {});
        let written = remote.write_ref(MAIN, Some(&w.old), &w.new);
        assert_eq!(intents(&remote), 1);
        let journal = remote.client.journal(&remote.slug).unwrap();
        let (records, _) = journal.read().unwrap();
        let own = records
            .iter()
            .find_map(|(_, record)| match record {
                JournalRecord::Intent(intent) => Some(intent.id.clone()),
                _ => None,
            })
            .unwrap();
        let seen = view(&remote, &own);
        assert!(matches!(seen.settled, Some(Settled::Unknown { .. })));
        if held {
            assert_eq!(written.unwrap(), RefWrite::Written);
            assert_eq!(w.main(), w.new);
            assert!(!seen.open);
            assert_eq!(
                lines(&said_lines),
                [format!(
                    "intent {own}: this write's answer was lost and origin holds every ref it \
                     asked for; it is recorded found"
                )]
            );
        } else {
            let error = written.unwrap_err();
            assert!(
                error.contains(&format!("outcome unknown; intent {own} open")),
                "{error}"
            );
            assert!(seen.open);
            assert!(lines(&said_lines).is_empty(), "no line was said");
        }
    }
}

/// The cut's main thread and its lease worker are threads of one process on one journal:
/// they take turns before they begin, so neither meets the other's open intent, and none of
/// them waits or says a line.
#[test]
fn the_threads_of_one_process_take_turns_at_one_journal() {
    let w = world();
    let (mut remote, fx) = w.remote();
    let said_lines = said(&mut remote, || {});
    remote.fetch_exact(&w.old).unwrap();
    let remote = &remote;
    std::thread::scope(|scope| {
        let writers: Vec<_> = (0..4)
            .map(|writer| {
                let (old, new) = (w.old.clone(), w.new.clone());
                scope.spawn(move || {
                    let name = format!("refs/heads/turn-{writer}");
                    [
                        remote.write_ref(&name, None, &old),
                        remote.write_ref(&name, Some(&old), &new),
                    ]
                })
            })
            .collect();
        for writer in writers {
            assert_eq!(
                writer.join().unwrap(),
                [Ok(RefWrite::Written), Ok(RefWrite::Written)]
            );
        }
    });
    assert_eq!(intents(remote), 8);
    assert!(lines(&said_lines).is_empty(), "no line was said");
    assert!(fx.slept().is_empty(), "nothing waited");
}

/// A checkout whose tracking ref another refresh moves while this one reads origin: during
/// each of its reads where `every`, during the first alone otherwise.
fn refreshing(every: bool) -> (World, Remote) {
    let w = world();
    w.land_new();
    let armed = Arc::new(AtomicBool::new(false));
    let moves = Arc::new(AtomicUsize::new(0));
    let (arming, moving, source) = (Arc::clone(&armed), Arc::clone(&moves), w.source.clone());
    let tree = format!("{}^{{tree}}", w.old);
    let fx = agh::Fx::simulated(1_760_000_000).with_git_child_observer_for_tests(move |_| {
        if !arming.load(Ordering::SeqCst) {
            return;
        }
        let moved = moving.fetch_add(1, Ordering::SeqCst);
        if every || moved == 0 {
            // Another refresh records its own reading: a commit this one did not read.
            let message = format!("another refresh {moved}");
            let other = git(&source, &["commit-tree", &tree, "-m", &message]);
            git(&source, &["update-ref", ORIGIN_MAIN, &other]);
        }
    });
    let remote = local_with_fx(&w.source, &w.bare, Arc::new(fx));
    armed.store(true, Ordering::SeqCst);
    (w, remote)
}

/// The third stop of the 0.200.0 cut's step 8: `cannot refresh origin/main for resume:
/// origin/main changed while its native refresh was running`. Another refresh of the same
/// checkout (the lease worker's, the pre-freeze observer's) recorded its reading during this
/// one. The refresh now reads origin again and goes on.
#[test]
fn a_refresh_reads_again_when_another_refresh_moved_the_tracking_ref() {
    let (w, remote) = refreshing(false);
    assert_eq!(remote.refresh_main().unwrap(), w.new);
    assert_eq!(git(&w.source, &["rev-parse", ORIGIN_MAIN]), w.new);

    let (w, remote) = refreshing(false);
    assert_eq!(remote.fetch_origin().unwrap().main, w.new);
    assert_eq!(git(&w.source, &["rev-parse", ORIGIN_MAIN]), w.new);
}

/// Negative control: a tracking ref that moves during every read ends the refresh at its
/// bound, saying how many times it read, with the other refresh's value left in place.
#[test]
fn a_refresh_whose_every_read_is_overtaken_refuses_saying_how_many_times_it_read() {
    let (w, remote) = refreshing(true);
    let error = remote.refresh_main().unwrap_err();
    assert_eq!(
        error,
        format!(
            "origin/main changed while its native refresh was running: another refresh of this \
             checkout moved it during each of {REFRESH_READS} reads of origin"
        )
    );
    let left = git(&w.source, &["rev-parse", ORIGIN_MAIN]);
    assert!(left != w.new && left != w.old, "{left}");
}
