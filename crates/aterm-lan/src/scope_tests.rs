// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc;

use aterm_uds::lan::ScopePlan;
use aterm_uds::lan::wire::{Frame, Reader};

use super::*;

/// A hang detector, never a latency budget.
const MUST_HAPPEN: Duration = Duration::from_secs(60);

/// A duty a test scripts: its clock, which only its sleep moves; a plan of one folder named
/// for the clock's second as it is made; and an apply that records the plan, first waits for
/// the test's go where the test holds one, and answers what the test scripted, else the plan's
/// folders excluded.
struct Scripted {
    now: Mutex<u64>,
    slept: Mutex<Vec<Duration>>,
    applied: Mutex<Vec<(u64, ScopePlan)>>,
    /// What the next applies answer, in order.
    answers: Mutex<std::collections::VecDeque<ScopeAnswer>>,
    /// Told as an apply starts, which then waits for `go`.
    entered: Mutex<Option<mpsc::Sender<()>>>,
    go: Mutex<Option<mpsc::Receiver<()>>>,
    /// What every plan answers instead of a folder.
    refuse: Option<ScopeAnswer>,
}

impl Scripted {
    fn at(now: u64) -> Self {
        Self {
            now: Mutex::new(now),
            slept: Mutex::new(Vec::new()),
            applied: Mutex::new(Vec::new()),
            answers: Mutex::new(std::collections::VecDeque::new()),
            entered: Mutex::new(None),
            go: Mutex::new(None),
            refuse: None,
        }
    }

    fn folder(second: u64) -> BTreeSet<PathBuf> {
        BTreeSet::from([PathBuf::from(format!("/Users//alice/made-at-{second}"))])
    }
}

impl ScopeDuty for Scripted {
    fn plan(&self, uid: u32) -> Result<ScopePlan, ScopeAnswer> {
        if let Some(answer) = &self.refuse {
            return Err(answer.clone());
        }
        Ok(ScopePlan {
            uid,
            folders: Self::folder(self.now()),
        })
    }

    fn apply(&self, plan: &ScopePlan) -> ScopeAnswer {
        if let Some(entered) = self.entered.lock().unwrap().take() {
            entered.send(()).unwrap();
            let go = self.go.lock().unwrap().take().unwrap();
            go.recv_timeout(MUST_HAPPEN).unwrap();
        }
        self.applied
            .lock()
            .unwrap()
            .push((self.now(), plan.clone()));
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| ScopeAnswer::Excluded(u32::try_from(plan.folders.len()).unwrap()))
    }

    fn now(&self) -> u64 {
        *self.now.lock().unwrap()
    }

    fn sleep(&self, period: Duration) {
        self.slept.lock().unwrap().push(period);
        *self.now.lock().unwrap() += period.as_secs();
    }
}

fn scope(duty: &Arc<Scripted>) -> Scope {
    Scope::of(Duties {
        scope: Some(Arc::clone(duty) as Arc<dyn ScopeDuty>),
    })
}

/// A request inside the pace waits out the rest of the minute since the last apply, then plans
/// again: the apply takes the plan made after the wait, at the clock's second after it.
/// NEGATIVE CONTROL: answering a request inside the pace without waiting (busy, or an apply of
/// the plan made before the wait) fails it.
#[test]
fn a_request_inside_the_pace_waits_out_the_rest_and_plans_again() {
    let duty = Arc::new(Scripted::at(1_000));
    let scope = scope(&duty);
    assert_eq!(decide(&scope, 501), ScopeAnswer::Excluded(1));
    assert!(duty.slept.lock().unwrap().is_empty());
    *duty.now.lock().unwrap() = 1_030;
    assert_eq!(decide(&scope, 501), ScopeAnswer::Excluded(1));
    assert_eq!(*duty.slept.lock().unwrap(), [Duration::from_secs(30)]);
    let applied = duty.applied.lock().unwrap();
    assert_eq!(applied.len(), 2);
    assert_eq!(applied[1].0, 1_060, "the apply waited out the pace");
    assert_eq!(
        applied[1].1.folders,
        Scripted::folder(1_060),
        "a fresh plan"
    );
    drop(applied);
    // Past the pace nothing waits.
    *duty.now.lock().unwrap() = 1_200;
    assert_eq!(decide(&scope, 501), ScopeAnswer::Excluded(1));
    assert_eq!(duty.slept.lock().unwrap().len(), 1);
}

/// Only an apply that may have restarted `mds` starts the pace: after a busy apply, one that left
/// Spotlight as it is, and one that excluded nothing, the next request waits nothing; after an
/// exclusion of a folder or a failure, it waits out the rest of the minute.
/// NEGATIVE CONTROL: an apply of any answer starting the pace makes the request after a busy one
/// wait 50 s.
#[test]
fn a_busy_or_unchanged_apply_restarts_no_pace() {
    for (answer, waits) in [
        (ScopeAnswer::Busy, false),
        (ScopeAnswer::Unchanged("the receipt is full".into()), false),
        (ScopeAnswer::Excluded(0), false),
        (ScopeAnswer::Excluded(1), true),
        (ScopeAnswer::Failed("the edit failed".into()), true),
    ] {
        let duty = Arc::new(Scripted::at(1_000));
        duty.answers.lock().unwrap().push_back(answer.clone());
        let scope = scope(&duty);
        assert_eq!(decide(&scope, 501), answer);
        *duty.now.lock().unwrap() = 1_010;
        assert_eq!(decide(&scope, 501), ScopeAnswer::Excluded(1), "{answer:?}");
        let slept = duty.slept.lock().unwrap().clone();
        if waits {
            assert_eq!(slept, [Duration::from_secs(50)], "{answer:?}");
        } else {
            assert!(slept.is_empty(), "{answer:?}: {slept:?}");
        }
    }
}

/// While one request applies, another is answered busy at once; the first still finishes.
/// NEGATIVE CONTROL: waiting for the lock (`lock` in place of `try_lock`) leaves the second
/// request unanswered until the first ends, which the hang detector fails.
#[test]
fn a_request_while_another_applies_is_busy_at_once() {
    let duty = Arc::new(Scripted::at(1_000));
    let (entered, apply_started) = mpsc::channel();
    let (go, released) = mpsc::channel();
    *duty.entered.lock().unwrap() = Some(entered);
    *duty.go.lock().unwrap() = Some(released);
    let scope = Arc::new(scope(&duty));
    let first = {
        let scope = Arc::clone(&scope);
        std::thread::spawn(move || decide(&scope, 501))
    };
    apply_started.recv_timeout(MUST_HAPPEN).unwrap();
    let (answered, second) = mpsc::channel();
    {
        let scope = Arc::clone(&scope);
        std::thread::spawn(move || answered.send(decide(&scope, 501)).unwrap());
    }
    assert_eq!(second.recv_timeout(MUST_HAPPEN).unwrap(), ScopeAnswer::Busy);
    go.send(()).unwrap();
    assert_eq!(first.join().unwrap(), ScopeAnswer::Excluded(1));
}

/// A lock a panicked request left poisoned still serves the next request.
/// NEGATIVE CONTROL: answering a poisoned lock busy fails it.
#[test]
fn a_poisoned_lock_still_serves() {
    let duty = Arc::new(Scripted::at(1_000));
    let scope = Arc::new(scope(&duty));
    {
        let scope = Arc::clone(&scope);
        let panicked = std::thread::spawn(move || {
            let _held = scope.held.lock().unwrap();
            panic!("a request's apply panicked");
        })
        .join();
        assert!(panicked.is_err());
    }
    assert!(scope.held.is_poisoned());
    assert_eq!(decide(&scope, 501), ScopeAnswer::Excluded(1));
}

/// A broker with no duty answers every request unserved, and the duty's own refusal ends a
/// request with no apply.
/// NEGATIVE CONTROL: a broker with no duty that answers busy fails the first half.
#[test]
fn no_duty_is_unserved_and_a_refused_plan_applies_nothing() {
    assert!(matches!(
        decide(&Scope::none(), 501),
        ScopeAnswer::Unserved(words) if words.contains("no Spotlight duty")
    ));
    let duty = Arc::new(Scripted {
        refuse: Some(ScopeAnswer::Refused("not the console's".into())),
        ..Scripted::at(1_000)
    });
    assert_eq!(
        decide(&scope(&duty), 501),
        ScopeAnswer::Refused("not the console's".into())
    );
    assert!(duty.applied.lock().unwrap().is_empty());
}

thread_local! {
    static LINES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn record(words: std::fmt::Arguments<'_>) {
    LINES.with_borrow_mut(|lines| lines.push(words.to_string()));
}

/// The request's one log line names the peer's pid and uid and the answer's class, never a
/// path or the duty's words; the client hears the answer's own frame.
/// NEGATIVE CONTROL: logging the answer itself puts the duty's words, a path among them, in
/// the line.
#[test]
fn the_log_line_names_the_class_and_no_path() {
    let duty = Arc::new(Scripted {
        refuse: Some(ScopeAnswer::Refused(
            "/Users//alice is not this account's own folder".into(),
        )),
        ..Scripted::at(1_000)
    });
    let (server, client) = CtlStream::pair().unwrap();
    serve(&scope(&duty), &server, record).unwrap();
    let mut reader = Reader::new(0);
    assert_eq!(
        reader
            .frame(&client, Some(std::time::Instant::now() + MUST_HAPPEN))
            .unwrap(),
        Some(Frame::Error {
            code: libc::EPERM,
            words: "/Users//alice is not this account's own folder".into(),
        })
    );
    let uid = aterm_uds::peer::our_uid();
    LINES.with_borrow(|lines| {
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].starts_with("scope pid "), "{lines:?}");
        assert!(
            lines[0].ends_with(&format!(" uid {uid} refused")),
            "{lines:?}"
        );
        assert!(!lines[0].contains('/'), "{lines:?}");
    });
}

/// Adoption's exit takes the lock for good: while an apply holds it the take does not happen
/// (its bound passes first), and once it is taken a request is answered busy, so no edit starts
/// as the process ends.
/// NEGATIVE CONTROL: a settle that reports the lock taken without taking it fails both
/// halves.
#[test]
fn adoption_waits_for_an_apply_and_keeps_the_lock() {
    let applying = scope(&Arc::new(Scripted::at(1_000)));
    let held = applying.held.lock().unwrap();
    assert!(!settle(&applying.held, Duration::from_millis(50)));
    drop(held);

    let duty = Arc::new(Scripted::at(1_000));
    let idle = scope(&duty);
    assert!(settle(&idle.held, MUST_HAPPEN));
    assert_eq!(decide(&idle, 501), ScopeAnswer::Busy);
    assert!(duty.applied.lock().unwrap().is_empty());
}
