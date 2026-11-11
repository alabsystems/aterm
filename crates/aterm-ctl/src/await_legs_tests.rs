// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn parts(line: &str) -> Vec<String> {
    line.split_whitespace().map(String::from).collect()
}

#[test]
fn the_inbox_and_agent_waits_are_parsed_with_their_timeout_taken_out() {
    assert_eq!(
        parse(&parts("@s-ab await inbox since=4 topic=lab timeout=900000")),
        Some(Wait {
            parts: parts("@s-ab await inbox since=4 topic=lab"),
            timeout: Some(Duration::from_secs(900)),
        })
    );
    assert_eq!(
        parse(&parts("await inbox timeout 5000 re=7")),
        Some(Wait {
            parts: parts("await inbox re=7"),
            timeout: Some(Duration::from_secs(5)),
        })
    );
    assert_eq!(
        parse(&parts("await inbox since=4")),
        Some(Wait {
            parts: parts("await inbox since=4"),
            timeout: None,
        })
    );
    assert_eq!(
        parse(&parts(
            "@s-ab await agent idle,prompt since=7 timeout=900000"
        )),
        Some(Wait {
            parts: parts("@s-ab await agent idle,prompt since=7"),
            timeout: Some(Duration::from_secs(900)),
        })
    );
    assert_eq!(
        parse(&parts("@s-ab await edit ticket=7 timeout=900000")),
        Some(Wait {
            parts: parts("@s-ab await edit ticket=7"),
            timeout: Some(Duration::from_secs(900)),
        })
    );
    assert_eq!(
        parse(&parts("await edit uri=file:///tmp/x")),
        Some(Wait {
            parts: parts("await edit uri=file:///tmp/x"),
            timeout: None,
        })
    );
    for refused in [
        "await idle 500",
        "await task @s-a",
        "await gone ready",
        "inbox since=4",
        "agent idle",
        "await inbox since=4 timeout=soon",
        "await inbox since=4 timeout=+5000",
        "await inbox since=4 timeout +5000",
        "await inbox since=4 timeout=1 timeout=2",
    ] {
        assert_eq!(parse(&parts(refused)), None, "{refused}");
    }
}

struct Run {
    sent: Vec<(String, Duration)>,
    printed: Vec<String>,
    code: ExitCode,
}

fn run(wait: &Wait, until: Option<Instant>, script: Vec<Leg>) -> Run {
    let mut script = script.into_iter();
    let mut sent = Vec::new();
    let mut printed = Vec::new();
    let code = {
        let mut leg = |_: &str, _: TargetOrigin, request: &str, asked: Duration| {
            sent.push((request.to_string(), asked));
            let next = script.next().unwrap_or(Leg::TimedOut);
            if matches!(next, Leg::TimedOut) {
                std::thread::sleep(asked);
            }
            next
        };
        let mut out = |line: &str| {
            printed.push(line.to_string());
            Ok(())
        };
        wait_legs(
            wait,
            "/no/such.sock",
            TargetOrigin::Pinned,
            until,
            &redial::Redial::none(),
            &mut leg,
            &mut out,
        )
        .expect("no transport error")
    };
    Run {
        sent,
        printed,
        code,
    }
}

#[test]
fn a_wait_is_re_armed_with_the_same_since_until_a_leg_answers() {
    let wait = parse(&parts("@s-ab await inbox since=4 topic=lab")).expect("re-armed");
    let until = Some(Instant::now() + Duration::from_secs(3600));
    let mut sent = Vec::new();
    let mut printed = Vec::new();
    let mut script = vec![
        Leg::TimedOut,
        Leg::TimedOut,
        Leg::Answered("OK inbox 9".into()),
    ]
    .into_iter();
    let code = wait_legs(
        &wait,
        "/no/such.sock",
        TargetOrigin::Pinned,
        until,
        &redial::Redial::none(),
        &mut |_: &str, _: TargetOrigin, request: &str, asked: Duration| {
            sent.push((request.to_string(), asked));
            script.next().expect("no leg past the answer")
        },
        &mut |line: &str| {
            printed.push(line.to_string());
            Ok(())
        },
    )
    .expect("no transport error");
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(printed, ["OK inbox 9"]);
    assert_eq!(sent.len(), 3);
    for (request, asked) in &sent {
        assert_eq!(
            request,
            "@s-ab await inbox since=4 topic=lab timeout=600000\n"
        );
        assert_eq!(*asked, await_task::LEG_MAX);
    }
}

#[test]
fn the_deadline_ends_the_wait_and_a_refusal_ends_it_at_once() {
    let wait = parse(&parts("await inbox since=4")).expect("re-armed");
    let short = Duration::from_millis(50);
    let got = run(&wait, Some(Instant::now() + short), Vec::new());
    assert_eq!(got.code, ExitCode::from(EXIT_TIMEOUT));
    assert_eq!(got.printed, ["OK timeout"]);
    assert_eq!(got.sent.len(), 1, "{:?}", got.sent);
    assert!(got.sent[0].1 <= short, "{:?}", got.sent);

    let got = run(&wait, None, vec![Leg::Code(ExitCode::FAILURE)]);
    assert_eq!((got.code, got.sent.len()), (ExitCode::FAILURE, 1));
    assert!(got.printed.is_empty());

    let mut leg = |_: &str, _: TargetOrigin, _: &str, _: Duration| Leg::HungUp {
        server: None,
        listener: None,
        error: None,
    };
    let cut = wait_legs(
        &wait,
        "/no/such.sock",
        TargetOrigin::Pinned,
        None,
        &redial::Redial::none(),
        &mut leg,
        &mut |_: &str| Ok(()),
    );
    assert_eq!(cut.map_err(|e| e.kind()), Err(io::ErrorKind::UnexpectedEof));
    assert!(
        run_rearmed_await(
            &parts("await inbox timeout=18446744073709551615"),
            "/no/such/aterm-ctl-test.sock",
            TargetOrigin::Pinned,
            &redial::Redial::none(),
            Some(Some(Duration::from_secs(u64::MAX))),
        )
        .is_err()
    );
}
