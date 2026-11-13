// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::mpsc;

const ID: &str = "0123456789abcdef0123456789abcdef";
const OTHER: &str = "1123456789abcdef0123456789abcdef";

fn output(code: i32) -> Output {
    Output {
        code,
        stdout: "a complete pass\n".into(),
        stderr: String::new(),
    }
}

#[test]
fn only_canonical_request_ids_reach_the_worker() {
    assert_eq!(
        Request::parse(&format!("start={ID}")),
        Ok(Request::Start(ID))
    );
    assert_eq!(Request::parse(&format!("poll={ID}")), Ok(Request::Poll(ID)));
    for rest in [
        String::new(),
        "start=".into(),
        format!("start={ID} trailing"),
        format!("poll={ID}\nstart={OTHER}"),
        format!("run={ID}"),
        format!("start={}", ID.to_uppercase()),
        format!("start={ID}0"),
    ] {
        assert!(Request::parse(&rest).is_err(), "{rest:?}");
    }
}

#[test]
fn paused_remote_work_is_retained_until_the_admitted_worker_finishes() {
    let book = Arc::new(Book::default());
    let lifetime = super::super::account_work::Lifetime::default();
    let (ready, began) = mpsc::channel();
    let (release, finish) = mpsc::channel();
    assert!(matches!(
        book.start(&lifetime, "a", ID, move |scope| {
            ready.send(()).unwrap();
            finish.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
            assert!(scope.cancelled());
            Ok(Some(output(0)))
        }),
        Ok(State::Working)
    ));
    began.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
    let pause = lifetime
        .pause()
        .unwrap()
        .try_wait()
        .expect_err("remote worker is retained");
    assert!(matches!(
        book.start(&lifetime, "a", ID, |_| panic!("duplicate work")),
        Ok(State::Working)
    ));
    assert!(matches!(
        book.start(&lifetime, "a", OTHER, |_| panic!("paused work")),
        Ok(State::Unavailable)
    ));
    assert!(book.poll("a", OTHER).is_err());
    release.send(()).unwrap();
    let mut pause = Some(pause);
    let drained = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(1),
        || match pause.take().unwrap().try_wait() {
            Ok(drained) => Some(drained),
            Err(pending) => {
                pause = Some(pending);
                None
            }
        },
    )
    .expect("remote worker retired");
    assert!(matches!(book.poll("a", ID), Ok(State::Unavailable)));
    assert!(drained.resume().is_ok());
}

#[test]
fn duplicates_share_one_run_and_callers_cannot_read_each_others_results() {
    let book = Arc::new(Book::default());
    let lifetime = super::super::account_work::Lifetime::default();
    let (release, wait) = mpsc::channel();
    assert!(matches!(
        book.start(&lifetime, "a", ID, move |_| {
            wait.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
            Ok(Some(output(7)))
        }),
        Ok(State::Working)
    ));
    assert!(matches!(
        book.start(&lifetime, "a", ID, |_| panic!("duplicate execution")),
        Ok(State::Working)
    ));
    assert!(book.poll("b", ID).is_err());
    assert!(
        book.start(&lifetime, "b", OTHER, |_| panic!("parallel execution"))
            .is_err()
    );
    release.send(()).unwrap();
    let done = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(1),
        || match book.poll("a", ID) {
            Ok(State::Done(value)) => Some(value),
            _ => None,
        },
    )
    .expect("the owned worker finished");
    assert_eq!(done.code, 7);
    assert!(matches!(
        book.start(&lifetime, "a", ID, |_| panic!("completed replay")),
        Ok(State::Done(_))
    ));
    assert!(book.poll("b", ID).is_err());
}

#[test]
fn an_error_finishes_the_request_and_does_not_hold_the_worker_slot() {
    let book = Arc::new(Book::default());
    let lifetime = super::super::account_work::Lifetime::default();
    book.start(&lifetime, "a", ID, |_| Err("no local route".into()))
        .ok()
        .unwrap();
    aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(1),
        || matches!(book.poll("a", ID), Ok(State::Failed(_))).then_some(()),
    )
    .expect("the refused worker finished");
    book.start(&lifetime, "a", OTHER, |_| Ok(Some(output(0))))
        .ok()
        .unwrap();
    aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(1),
        || matches!(book.poll("a", OTHER), Ok(State::Done(_))).then_some(()),
    )
    .expect("another request used the released slot");
}

#[test]
fn complete_results_preserve_nonzero_exits_and_unknown_states_never_mean_success() {
    let complete = json!({"schema":1,"request":ID,"state":"done","code":9,
        "stdout":"not renewed\n","stderr":"provider refused\n"})
    .to_string();
    let State::Done(got) = decode(ID, &complete).ok().unwrap() else {
        panic!("complete")
    };
    assert_eq!(got.code, 9);
    assert_eq!(got.stderr, "provider refused\n");
    for change in ["schema", "request", "state", "code", "stdout"] {
        let mut bad: Value = aterm_json::from_str(&complete).unwrap();
        let value = match change {
            "schema" | "code" => json!(-1),
            "request" => json!(OTHER),
            "state" => json!("unknown"),
            _ => json!("x".repeat(MAX_OUTPUT + 1)),
        };
        bad.as_object_mut().unwrap().insert(change.into(), value);
        assert!(decode(ID, &bad.to_string()).is_err(), "{change}");
    }
}

#[test]
fn a_lost_start_or_poll_never_restarts_the_sign_in() {
    for lose_start in [true, false] {
        let mut calls = Vec::new();
        let result = request_with(
            ID,
            Duration::from_secs(60),
            |line, _| {
                calls.push(line.to_owned());
                if lose_start || calls.len() > 1 {
                    Err("connection closed".into())
                } else {
                    Ok(json!({"schema":1,"request":ID,"state":"working"}).to_string())
                }
            },
            |_| {},
        );
        assert!(result.is_err());
        assert_eq!(
            calls.iter().filter(|line| line.contains("start=")).count(),
            1
        );
        assert_eq!(calls.len(), 2);
    }
}

#[test]
fn the_client_waits_for_a_complete_result_on_the_same_request() {
    let mut calls = Vec::new();
    let result = request_with(
        ID,
        Duration::from_secs(60),
        |line, within| {
            assert!(within <= DIAL_WITHIN);
            calls.push(line.to_owned());
            Ok(if calls.len() == 1 {
                json!({"schema":1,"request":ID,"state":"working"})
            } else {
                json!({"schema":1,"request":ID,"state":"done","code":0,
                "stdout":"nothing is due on this machine\n","stderr":""})
            }
            .to_string())
        },
        |_| {},
    )
    .ok()
    .unwrap();
    assert_eq!(result.code, 0);
    assert_eq!(
        calls,
        [
            format!("accounts-renew start={ID}"),
            format!("accounts-renew poll={ID}")
        ]
    );
}

#[test]
fn a_lost_start_reply_is_recovered_by_polling_without_another_start() {
    let mut calls = Vec::new();
    let result = request_with(
        ID,
        Duration::from_secs(60),
        |line, _| {
            calls.push(line.to_owned());
            if calls.len() == 1 {
                Err("reply lost after the peer accepted".into())
            } else {
                Ok(json!({"schema":1,"request":ID,"state":"done","code":0,
                "stdout":"completed once\n","stderr":""})
                .to_string())
            }
        },
        |_| {},
    )
    .ok()
    .unwrap();
    assert_eq!(result.stdout, "completed once\n");
    assert_eq!(
        calls,
        [
            format!("accounts-renew start={ID}"),
            format!("accounts-renew poll={ID}")
        ]
    );
}
