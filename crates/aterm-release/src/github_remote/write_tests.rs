// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::github_remote::tests::{begin_intent, commit, git, repo, writer};
use agh::error::{Kind, SideEffect};
use std::cell::RefCell;

#[test]
fn an_uncertain_native_write_is_never_a_lost_lease() {
    assert!(is_conflict(&agh::Error::new(Kind::Conflict, "changed")));
    for error in [
        agh::Error::new(Kind::Conflict, "changed").with_side_effect(SideEffect::Unknown),
        agh::Error::new(Kind::Conflict, "changed").with_side_effect(SideEffect::Applied),
        agh::Error::new(Kind::OutcomeUnknown, "non-fast-forward; [rejected]")
            .with_side_effect(SideEffect::Unknown),
        agh::Error::new(Kind::CouldNotRun, "fetch first"),
        agh::Error::new(Kind::Refused, "cannot lock ref 'refs/heads/main'"),
    ] {
        assert!(!is_conflict(&error));
    }
}

#[test]
fn only_a_settled_transport_failure_is_retryable() {
    assert!(is_transient(&agh::Error::could_not_run("HTTP 503")));
    for error in [
        agh::Error::could_not_run("HTTP 503").with_side_effect(SideEffect::Unknown),
        agh::Error::new(Kind::Conflict, "stale info"),
        agh::Error::refused("protected"),
    ] {
        assert!(!is_transient(&error));
    }
}

/// A remote on a real journal and a local bare origin, under the simulated clock, which moves
/// only when the write sleeps.
struct Fixture {
    _root: aterm_tempfile::TempDir,
    remote: Remote,
    fx: Arc<agh::Fx>,
}

fn fixture() -> Fixture {
    let root = aterm_tempfile::tempdir().unwrap();
    let bare = repo(root.path(), "remote.git", true);
    let source = repo(root.path(), "source", false);
    commit(&source, "remote main\n");
    git(&source, &["push", "-q", bare.to_str().unwrap(), "main"]);
    git(
        &source,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    let fx = Arc::new(agh::Fx::simulated(1_760_000_000));
    let remote = claim_fixture_with_fx(&source, &bare, Arc::clone(&fx));
    Fixture {
        _root: root,
        remote,
        fx,
    }
}

/// Begin another writer's intent that records its batch, as agh's native push records one,
/// with `writer` as its custody.
fn begin(remote: &Remote, writer: agh::journal::IntentWriter) -> agh::IntentId {
    begin_intent(
        remote,
        "sha256:another-writers-push 162",
        Some(writer),
        vec![RefUpdate {
            reference: "refs/heads/main".into(),
            before: "1".repeat(40),
            after: "2".repeat(40),
        }],
    )
}

/// agh's refusal of a write behind the open `intent`, which sent nothing: the words
/// `Refs::reconcile_dead_writer` gives where origin read the intent's refs neither wholly as
/// before its write nor wholly as after it, or could not read them. The cutter reads only its
/// kind, its intent and that nothing was sent ([`blocking_intent`]).
fn refusal(intent: &agh::IntentId) -> agh::Error {
    agh::Error::new(
        Kind::OutcomeUnknown,
        format!("intent {intent} has dead writer; origin is mixed or unreadable"),
    )
    .with_sub(agh::Sub::IntentOpen {
        intent: intent.to_string(),
    })
    .with_repo("fixture/app")
    .with_next(format!(
        "agh intent retract fixture/app {intent} --confirm {intent}"
    ))
}

/// The most pushes a write here may make: past them the push panics, so a write made again
/// without bound, which the simulated clock never ends, fails instead of running forever.
const FUSE: usize = 12;

/// Make a write behind `intent` through [`Remote::journaled`], whose every push agh refuses
/// ([`refusal`]), running `on_push` with each push's number first. It returns what the write
/// returned and the simulated time at which each push was made.
fn behind(
    f: &Fixture,
    intent: &agh::IntentId,
    stop: &dyn Fn() -> bool,
    on_push: &dyn Fn(usize),
) -> (Result<agh::Result<()>, String>, Vec<Duration>) {
    let pushes = RefCell::new(Vec::new());
    let asked = Asked {
        request: "sha256:this-write 162".into(),
        wanted: vec![("refs/heads/main".into(), Some("3".repeat(40)))],
        stop,
    };
    let result = f.remote.journaled(&asked, &|_| {
        let made = {
            let mut pushes = pushes.borrow_mut();
            assert!(
                pushes.len() < FUSE,
                "made {FUSE} times: nothing bounds the write"
            );
            pushes.push(f.fx.mono().unwrap());
            pushes.len()
        };
        on_push(made);
        Err(refusal(intent))
    });
    (result, pushes.into_inner())
}

/// Keep every line `remote` says, running `on_line` on each; a line of a wait or a pause is
/// evidence the lease heartbeat reads while it lasts, so each is held as that evidence while it
/// is said (that line itself: other tests' waits are held beside it).
fn said(
    remote: &mut Remote,
    on_line: impl Fn(&str) + Send + Sync + 'static,
) -> Arc<Mutex<Vec<String>>> {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&lines);
    remote.notice = Box::new(move |line| {
        kept.lock().unwrap().push(line.to_owned());
        assert!(
            intent_waits().iter().any(|held| held == line),
            "not held as a wait: {line}"
        );
        on_line(line);
    });
    lines
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

/// Each pause's line, in order.
fn paced(intent: &agh::IntentId) -> Vec<String> {
    (1..=RECONCILE_RETRIES)
        .map(|repeat| {
            format!(
                "intent {intent}'s writer is gone and origin did not settle it; this write sent \
                 nothing and is made again in {} s ({repeat} of {RECONCILE_RETRIES})",
                RECONCILE_PAUSE.as_secs() << (repeat - 1)
            )
        })
        .collect()
}

/// The simulated times of a write's pushes behind a gone writer's intent: the first two at
/// once (agh's first push may not have read the intent from origin), then each after its pause.
fn pushed_at(first: Duration) -> Vec<Duration> {
    let mut at = vec![first, first];
    for repeat in 0..RECONCILE_RETRIES {
        let last = at[at.len() - 1];
        at.push(last + RECONCILE_PAUSE * (1 << repeat));
    }
    at
}

/// RL-21: an intent whose writer is gone stays open until a push reads its batch on origin
/// wholly as before or wholly as after its write, and agh refuses the same way while origin is
/// mixed or unreadable. A write behind one was made again at once, every time, until five
/// minutes passed; on this clock, which moves only when the write sleeps, it never ended. It
/// is now made again at once and then after pauses of 2, 4, 8, 16 and 32 s on the write's own
/// clock, each in polls and each said in one line, and then ends refused, naming the intent,
/// `agh intent show` and agh's `agh intent retract`: seven pushes, and the intent stays open.
/// Negative control: the old loop reaches the fuse.
#[test]
fn a_write_behind_a_gone_writers_intent_origin_does_not_settle_pauses_doubling_then_refuses() {
    let mut f = fixture();
    let gone = begin(&f.remote, writer(None));
    let said_lines = said(&mut f.remote, |_| {});
    let (result, pushes) = behind(&f, &gone, &|| false, &|_| {});
    let error = result.unwrap_err();
    for words in [
        format!("agh intent retract fixture/app {gone} --confirm {gone}"),
        format!(
            "intent {gone}'s writer is gone, and the {} pushes made since each found its refs on \
             origin neither wholly as before nor wholly as after its write, or could not read \
             them; nothing was sent; `agh intent show fixture/app {gone}` shows its batch",
            RECONCILE_RETRIES + 1
        ),
    ] {
        assert!(error.contains(&words), "{error}");
    }
    assert_eq!(pushes, pushed_at(Duration::ZERO));
    let slept = f.fx.slept();
    assert!(slept.iter().all(|look| *look == INTENT_POLL), "{slept:?}");
    assert_eq!(*said_lines.lock().unwrap(), paced(&gone));
    assert!(view(&f.remote, &gone).open, "nothing closed it");
}

/// A stopped monitor (`stop`) or a signal that interrupted the release ends a write behind a
/// gone writer's intent with no further push: stopped from the start, after its one push;
/// stopped inside its first pause, at the next look; and stopped as that pause's last slice
/// ends, before the push it would have made next. Negative control: before, nothing on this
/// path read either, and each case reached the fuse.
#[test]
fn a_stopped_or_interrupted_write_behind_a_gone_writers_intent_makes_no_further_push() {
    for signal in [None, Some(15)] {
        for (looks, pushes_made) in [(0, 1), (1, 2), (4, 2)] {
            let mut f = fixture();
            let gone = begin(&f.remote, writer(None));
            let fx = Arc::clone(&f.fx);
            let ends = move || fx.slept().len() >= looks;
            let (result, pushes) = match signal {
                Some(signal) => {
                    f.remote.interrupted = Box::new(move || ends().then_some(signal));
                    behind(&f, &gone, &|| false, &|_| {})
                }
                None => behind(&f, &gone, &ends, &|_| {}),
            };
            let error = result.unwrap_err();
            let words = match signal {
                Some(signal) => {
                    format!("stopped by signal {signal} while waiting for intent {gone} to close")
                }
                None => format!("stopped while waiting for intent {gone} to close"),
            };
            assert!(error.contains(&words), "{error}");
            assert_eq!(
                pushes.len(),
                pushes_made,
                "{signal:?} after {looks} looks: {error}"
            );
            assert_eq!(f.fx.slept().len(), looks, "{signal:?}");
            assert!(view(&f.remote, &gone).open);
        }
    }
}

/// A signal that interrupted the release ends a wait behind a live writer's intent at the next
/// look, with no further push: caught before the first poll, the wait takes none; caught after
/// three, it takes three, not the 300 s of [`INTENT_WAIT`]; the intent stays open. Negative
/// control: where the poll read only `stop`, the wait ran its whole bound.
#[test]
fn an_interrupted_wait_behind_a_live_writers_intent_ends_at_the_next_look() {
    for looks in [0, 3] {
        let mut f = fixture();
        let live = begin(&f.remote, agh::journal::IntentWriter::current().unwrap());
        let fx = Arc::clone(&f.fx);
        f.remote.interrupted = Box::new(move || (fx.slept().len() >= looks).then_some(15));
        let (result, pushes) = behind(&f, &live, &|| false, &|_| {});
        let error = result.unwrap_err();
        assert!(
            error.contains(&format!(
                "stopped by signal 15 while waiting for intent {live} to close"
            )),
            "{error}"
        );
        assert_eq!(pushes.len(), 1, "after {looks} looks: {error}");
        assert_eq!(f.fx.slept().len(), looks);
        assert!(view(&f.remote, &live).open);
    }
}

/// A signal caught in the poll in which a live writer's wait ends, the intent closing or its
/// writer dying, ends the write there: the look after the last poll reads it, and no push is
/// made behind it. The end happens in the look before that poll, the one moment the wait hands
/// the other writer. Negative control: where nothing looked after the last poll, or before the
/// write was made again, a second push was made.
#[test]
fn a_signal_caught_in_the_poll_that_ends_a_live_writers_wait_makes_no_further_push() {
    use agh::IntentStore as _;
    const LOOKS: usize = 2;
    for closes in [true, false] {
        let mut f = fixture();
        let child = aterm_uds::spawnfd::command("/bin/cat")
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .unwrap();
        let held = begin(&f.remote, writer(Some(child.id())));
        let journal = f.remote.client.journal(&f.remote.slug).unwrap();
        let child = Arc::new(Mutex::new(Some(child)));
        let reap = move |child: &Mutex<Option<aterm_uds::spawnfd::Child>>| {
            if let Some(mut child) = child.lock().unwrap().take() {
                child.kill().unwrap();
                child.wait().unwrap();
            }
        };
        let (ending, dying) = (held.clone(), Arc::clone(&child));
        let end = move || {
            if closes {
                journal.observe(&ending, Some(0)).unwrap();
            } else {
                reap(&dying);
            }
        };
        let fx = Arc::clone(&f.fx);
        f.remote.interrupted = Box::new(move || {
            let looked = fx.slept().len();
            if looked == LOOKS - 1 {
                end();
            }
            (looked >= LOOKS).then_some(15)
        });
        let (result, pushes) = behind(&f, &held, &|| false, &|_| {});
        let error = result.unwrap_err();
        assert!(
            error.contains(&format!(
                "stopped by signal 15 while waiting for intent {held} to close"
            )),
            "{closes}: {error}"
        );
        assert_eq!(pushes.len(), 1, "{closes}: {error}");
        assert_eq!(f.fx.slept().len(), LOOKS, "{closes}");
        assert_eq!(view(&f.remote, &held).open, !closes);
        reap(&child);
    }
}

/// A refusal whose intent closed before the write looked at it (its writer settled it, or
/// another writer's origin read retracted or found it) is the case the write is made again for,
/// and a stopped monitor or an interrupted release still makes no further push: one push, the
/// stop's words, no pause. Negative control: where only an open intent's path read `stop` and
/// the interrupt, the write was made again, until the fuse.
#[test]
fn a_stopped_or_interrupted_write_whose_refusing_intent_closed_makes_no_further_push() {
    use agh::IntentStore as _;
    for signal in [None, Some(15)] {
        let mut f = fixture();
        let closed = begin(&f.remote, agh::journal::IntentWriter::current().unwrap());
        f.remote
            .client
            .journal(&f.remote.slug)
            .unwrap()
            .observe(&closed, Some(0))
            .unwrap();
        assert!(!view(&f.remote, &closed).open);
        let (result, pushes) = match signal {
            Some(signal) => {
                f.remote.interrupted = Box::new(move || Some(signal));
                behind(&f, &closed, &|| false, &|_| {})
            }
            None => behind(&f, &closed, &|| true, &|_| {}),
        };
        let error = result.unwrap_err();
        let words = match signal {
            Some(signal) => {
                format!("stopped by signal {signal} while waiting for intent {closed} to close")
            }
            None => format!("stopped while waiting for intent {closed} to close"),
        };
        assert!(error.contains(&words), "{error}");
        assert_eq!(pushes.len(), 1, "{signal:?}: {error}");
        assert!(f.fx.slept().is_empty(), "{signal:?}");
    }
}

/// The pauses stay inside the write's whole wait: where its pushes took 299 s of it, the next
/// pause is the 1 s left, and the refusal after it says the wait is spent. Negative control:
/// before, a refusal inside the wait made the write again at once, and it reached the fuse.
#[test]
fn a_pause_behind_a_gone_writers_intent_ends_with_the_writes_wait() {
    let mut f = fixture();
    let gone = begin(&f.remote, writer(None));
    let said_lines = said(&mut f.remote, |_| {});
    let slow = Duration::from_secs(299);
    let fx = Arc::clone(&f.fx);
    let (result, pushes) = behind(&f, &gone, &|| false, &move |made| {
        if made == 2 {
            fx.sleep(slow).unwrap();
        }
    });
    let error = result.unwrap_err();
    assert!(
        error.contains("other writers' intents kept this write from beginning for 300 s"),
        "{error}"
    );
    assert_eq!(pushes.len(), 3);
    assert_eq!(f.fx.slept(), [slow, INTENT_POLL, INTENT_POLL]);
    assert_eq!(
        *said_lines.lock().unwrap(),
        [format!(
            "intent {gone}'s writer is gone and origin did not settle it; this write sent \
             nothing and is made again in 1 s (1 of {RECONCILE_RETRIES})"
        )]
    );
}

/// A write that waited on a live writer's intent until that writer died paces the refusal of
/// its next push, which already read the intent from origin, as a repeat: its pushes come as
/// those of a write that found the writer gone at once, the first before its wait. The writer
/// here is a gone frontend whose native child lives until the wait begins. Negative control:
/// where the end of the wait is not recorded, that refusal makes the write again at once,
/// and an eighth push is made.
#[test]
fn a_wait_that_ends_on_its_writers_death_paces_the_next_refusal() {
    let mut f = fixture();
    let child = aterm_uds::spawnfd::command("/bin/cat")
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .unwrap();
    let dying = begin(&f.remote, writer(Some(child.id())));
    let child = Mutex::new(Some(child));
    let said_lines = said(&mut f.remote, move |line| {
        if line.starts_with("waiting for intent ")
            && let Some(mut child) = child.lock().unwrap().take()
        {
            child.kill().unwrap();
            child.wait().unwrap();
        }
    });
    let (result, pushes) = behind(&f, &dying, &|| false, &|_| {});
    let error = result.unwrap_err();
    assert!(
        error.contains(&format!(
            "intent {dying}'s writer is gone, and the {} pushes made since",
            RECONCILE_RETRIES + 1
        )),
        "{error}"
    );
    assert_eq!(pushes, pushed_at(Duration::ZERO));
    let mut lines = vec![format!(
        "waiting for intent {dying} of another writer on this Mac"
    )];
    lines.extend(paced(&dying));
    assert_eq!(*said_lines.lock().unwrap(), lines);
}

/// Control: a live writer's intent, though it records its batch, is waited on as before and
/// never paced or counted: one push, the whole wait in polls, and a refusal that says it is
/// still open.
#[test]
fn a_live_writers_intent_with_its_batch_is_waited_on_and_not_paced() {
    let f = fixture();
    let live = begin(&f.remote, agh::journal::IntentWriter::current().unwrap());
    let (result, pushes) = behind(&f, &live, &|| false, &|_| {});
    let error = result.unwrap_err();
    assert!(
        error.contains(&format!(
            "waited {} s for intent {live} to close and it is still open",
            INTENT_WAIT.as_secs()
        )),
        "{error}"
    );
    assert_eq!(pushes.len(), 1);
    assert!(f.fx.slept().iter().all(|look| *look == INTENT_POLL));
    assert_eq!(f.fx.slept().iter().sum::<Duration>(), INTENT_WAIT);
}
