// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn parts(line: &str) -> Vec<String> {
    line.split_whitespace().map(String::from).collect()
}

#[test]
fn the_client_reads_every_form_and_refuses_the_rest() {
    let list = parse(&parts(
        "await task @s-a1,@2:5,@s-a1 all since=3 timeout=1500",
    ))
    .unwrap();
    assert_eq!(
        list,
        Request {
            targets: vec![
                Target {
                    selector: "s-a1".into(),
                    since: None,
                    fact_since: None,
                },
                Target {
                    selector: "2".into(),
                    since: Some(5),
                    fact_since: None,
                },
            ],
            combine: Combine::All,
            since: Some(3),
            facts: false,
            fact_since: None,
            timeout: Some(Duration::from_millis(1500)),
        },
        "a target named twice is waited on once"
    );
    let single = parse(&parts("@s-b2 await task since=4 timeout 90000")).unwrap();
    assert_eq!(
        single.targets,
        vec![Target {
            selector: "s-b2".into(),
            since: None,
            fact_since: None,
        }]
    );
    assert_eq!(
        (single.since, single.timeout),
        (Some(4), Some(Duration::from_secs(90)))
    );
    let bare = parse(&parts("await task")).unwrap();
    assert_eq!(
        bare.targets,
        vec![Target {
            selector: ".".into(),
            since: None,
            fact_since: None,
        }]
    );
    assert_eq!(bare.combine, Combine::Any);
    assert!(!bare.facts);
    let facts = parse(&parts("await task @s-a:4:2,@s-b facts=1 fact_since=7")).unwrap();
    assert_eq!(
        (facts.facts, facts.fact_since),
        (true, Some(7)),
        "the manager door's opt-in"
    );
    assert_eq!(
        facts.targets,
        vec![
            Target {
                selector: "s-a".into(),
                since: Some(4),
                fact_since: Some(2),
            },
            Target {
                selector: "s-b".into(),
                since: None,
                fact_since: None,
            },
        ]
    );
    for bad in [
        "@s-a await task @s-b",
        "await task @s-a @s-b",
        "await task @s-a:x",
        "await task s-a",
        "await task any all",
        "await task since=1 since=2",
        "await task timeout=soon",
        "await task timeout",
        "await task later",
        "await task facts=1 facts=1",
        "await task facts=2",
        "await task facts=1 fact_since=1 fact_since=2",
        "await task facts=1 fact_since=x",
        "await task fact_since=1",
        "await task @s-a:4:2",
        "await task @s-a:4:x facts=1",
        "await task @s-a::2 facts=1",
        "await task @s-a:+4",
        "await task @s-a:4:+2 facts=1",
        "await task since=+1",
        "await task facts=1 fact_since=+1",
        "await task timeout=+1500",
        "await task timeout +1500",
    ] {
        assert!(parse(&parts(bad)).is_err(), "{bad:?}");
    }
    for empty in [
        "await task @s-a,@,@s-c",
        "await task @",
        "await task @:3",
        "await task @s-a,@:2",
    ] {
        let e = parse(&parts(empty)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{empty:?}");
        assert_eq!(
            e.to_string(),
            aterm_types::control_verbs::EMPTY_SELECTOR,
            "{empty:?}"
        );
    }
    let explicit = parse(&parts("await task @s-a,@.,@s-c")).unwrap();
    assert_eq!(
        explicit
            .targets
            .iter()
            .map(|t| t.selector.as_str())
            .collect::<Vec<_>>(),
        ["s-a", ".", "s-c"]
    );
    assert!(is_await_task(&parts("@s-a await task")));
    assert!(is_await_task(&parts("await task @s-a")));
    assert!(!is_await_task(&parts("await agent idle")));
    assert!(!is_await_task(&parts("@s-a await")));
    assert!(
        run_await_task(
            &parts("await task timeout=18446744073709551615"),
            true,
            "/no/such/aterm-ctl-test.sock",
            &redial::Redial::none(),
            Some(Some(Duration::from_secs(u64::MAX))),
        )
        .is_err()
    );
}

/// `failed` WAKES A WAIT like the other terminal words, past the edge the waiter last read.
/// NEGATIVE CONTROL: at or before its edge it is old news, and `working` is none.
#[test]
fn a_failed_row_is_news_past_its_edge() {
    let target = |since| Target {
        selector: "s-a".into(),
        since,
        fact_since: None,
    };
    let row = "@s-a rev=4 state=failed agent=claude program_state=- attention=- result_bytes=0 \
               conclusion=failed why=could-not-finish";
    assert!(row_news(row, &target(Some(3)), false));
    assert!(!row_news(row, &target(Some(4)), false));
    let working = row.replace("state=failed", "state=working");
    assert!(!row_news(&working, &target(Some(3)), false));
}

#[test]
fn a_leg_names_each_targets_baseline() {
    let targets = vec![
        Target {
            selector: "s-a".into(),
            since: Some(3),
            fact_since: None,
        },
        Target {
            selector: "1".into(),
            since: None,
            fact_since: None,
        },
    ];
    assert_eq!(
        leg_request(
            targets.iter(),
            Combine::All,
            false,
            Duration::from_secs(600)
        ),
        "await task @s-a:3,@1 all timeout=600000\n"
    );
    let mut facts = targets.clone();
    assert_eq!(
        leg_request(facts.iter(), Combine::Any, true, Duration::ZERO),
        "await task @s-a:3,@1 any facts=1 timeout=0\n"
    );
    facts[0].fact_since = Some(2);
    facts[1].fact_since = Some(5);
    assert_eq!(
        leg_request(facts.iter(), Combine::Any, true, Duration::ZERO),
        "await task @s-a:3:2,@1 any facts=1 fact_since=5 timeout=0\n",
        "a target with no rev yet takes the request's fact edge"
    );
    facts[1].since = Some(9);
    assert_eq!(
        leg_request(facts.iter(), Combine::Any, true, Duration::ZERO),
        "await task @s-a:3:2,@1:9:5 any facts=1 timeout=0\n"
    );
    assert_eq!(row_fact_rev("@s-a rev=7 fact=- fact_rev=3"), 3);
    assert_eq!(row_fact_rev("@s-a rev=7 state=done"), 0, "never edited");
    let row = "@s-a rev=7 state=done agent=- program_state=idle attention=- result_bytes=1";
    assert_eq!(row_rev(row), Some(7));
    assert_eq!(row_rev("garbage"), None);
    assert_eq!(row_sid(row), Some("s-a"));
    assert_eq!(row_sid("@3 rev=1"), None, "a row names a global sid");
    assert!(!row_closed(row));
    assert_eq!(
        closed_row(Some(row), &targets[0]),
        "@s-a rev=7 state=closed agent=- program_state=idle attention=- result_bytes=1 \
         task_state=done",
        "the server's last row stands, its program's report and its record's state with it"
    );
    assert_eq!(
        closed_row(None, &targets[0]),
        "@s-a rev=3 state=closed agent=- program_state=- attention=- result_bytes=0",
        "a row the client writes has every column a server row has"
    );
    assert!(row_closed(&closed_row(None, &targets[0])));
    assert!(
        row_field(&closed_row(None, &targets[0]), "task_news").is_none(),
        "a row the client writes says so by having no `task_news=`, which the board reads \
         as a rev no worker reported"
    );
    let refusal = io::Error::other("ERR no such session @s-a");
    assert_eq!(gone_target(&refusal, &targets, &[0, 1]), Some(0));
    assert_eq!(gone_target(&refusal, &targets, &[1]), None, "not live");
    let other = io::Error::other("ERR denied");
    assert_eq!(gone_target(&other, &targets, &[0, 1]), None);
}

/// A close a wait builds from the last row the worker's Mac answered keeps that row's rev and
/// `task_news=`, so a board takes the rev as one its worker reported: a live row read before
/// the close, at a lower rev, does not reopen the row and drop the conclusion and receipt the
/// close kept (the loss 6a8935aa23 stopped for a close the Mac wrote, which dropping
/// `task_news=` here would bring back on this path), and the worker's own row at the close's
/// rev still reopens it. NEGATIVE CONTROL: the close a wait builds from no row carries the
/// asker's edge and no `task_news=`, which leaves the board's rev, so the worker's own answer
/// below that edge still reopens the row.
#[test]
fn a_close_from_the_macs_last_row_keeps_its_rev_and_one_from_none_does_not() {
    let target = Target {
        selector: "s-0a1".into(),
        since: Some(9),
        fact_since: None,
    };
    let open = |rev: u64| crate::board::Row {
        rev,
        ..crate::board::Row::started("s-0a1", 1)
    };
    let last = "@s-0a1 rev=5 state=done agent=- program_state=- attention=- result_bytes=4 \
                conclusion=finished checked=0123456789ab@origin/main task_news=0";
    let closed = closed_row(Some(last), &target);
    assert_eq!(
        (row_rev(&closed), row_field(&closed, "task_news")),
        (Some(5), Some("1")),
        "the Mac's rev, marked as the Mac's: {closed}"
    );
    let mut row = open(3);
    assert!(row.merge(&closed, 10));
    assert_eq!(
        (row.state.as_str(), row.rev, row.landed.as_deref()),
        ("closed", 5, Some("0123456789ab"))
    );
    assert!(
        !row.merge(
            "@s-0a1 rev=4 state=working agent=- program_state=- attention=- result_bytes=0 \
             task_news=0",
            20
        ),
        "a read from before the close changes nothing"
    );
    assert_eq!(
        (
            row.state.as_str(),
            row.conclusion.as_deref(),
            row.checked.as_deref()
        ),
        ("closed", Some("finished"), Some("0123456789ab@origin/main"))
    );
    assert!(
        row.merge(last, 30),
        "the worker's own row at the close's rev reopens it: its tab came back"
    );
    assert_eq!((row.state.as_str(), row.rev), ("done", 5));

    let gone = closed_row(None, &target);
    assert_eq!(
        (row_rev(&gone), row_field(&gone, "task_news")),
        (Some(9), None),
        "the asker's edge, unmarked: {gone}"
    );
    let mut row = open(3);
    assert!(row.merge(&gone, 10));
    assert_eq!(
        (row.state.as_str(), row.rev),
        ("closed", 3),
        "an edge no worker reported is not the board's rev"
    );
    assert!(
        row.merge(
            "@s-0a1 rev=3 state=working agent=- program_state=- attention=- result_bytes=0 \
             task_news=0",
            20
        ),
        "the worker's own answer at its own rev reopens it"
    );
    assert_eq!(row.state, "working");
}

#[test]
fn a_legs_rows_share_one_byte_budget() {
    use std::io::Cursor;
    let cut = || Leg::Again(io::Error::from(io::ErrorKind::UnexpectedEof));
    let body = "one\r\r\nsecond\n";
    let wire = format!("OK 2 status=timeout\n{body}");
    let Leg::TimedOut(rows) = read_leg(&mut Cursor::new(wire.as_bytes()), body.len(), cut) else {
        panic!("the exact byte boundary must fit");
    };
    assert_eq!(rows, ["one", "second"]);
    let Leg::Failed(error) = read_leg(&mut Cursor::new(wire.as_bytes()), body.len() - 1, cut)
    else {
        panic!("the next row must be refused before it is retained");
    };
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    let Leg::Answered(rows) = read_leg(&mut Cursor::new(b"OK 0\n"), 0, cut) else {
        panic!("a zero-row answer fits no budget");
    };
    assert!(rows.is_empty());
    for partial in ["", "OK 2\none\n"] {
        assert!(matches!(
            read_leg(&mut Cursor::new(partial.as_bytes()), body.len(), cut),
            Leg::Again(_)
        ));
    }
    for malformed in ["OK nope\n", "OK 200001\n"] {
        let Leg::Failed(error) = read_leg(&mut Cursor::new(malformed.as_bytes()), body.len(), cut)
        else {
            panic!("a malformed count is a refusal, not a cut");
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
    let Leg::Failed(error) = read_leg(&mut Cursor::new(b"ERR denied\n"), body.len(), cut) else {
        panic!("the peer's refusal must remain a refusal");
    };
    assert_eq!(error.to_string(), "ERR denied");
}

type Reply = Box<dyn FnOnce() -> Leg + Send>;
type Step = (String, Reply);

fn scripted(script: Vec<(&'static str, &'static str, Reply)>) -> Legs {
    let mut by_sock: std::collections::HashMap<String, std::collections::VecDeque<Step>> =
        std::collections::HashMap::new();
    for (sock, request, step) in script {
        by_sock
            .entry(sock.to_string())
            .or_default()
            .push_back((request.to_string(), step));
    }
    let by_sock = std::sync::Mutex::new(by_sock);
    Legs {
        max: Duration::from_millis(600),
        again: (Duration::from_millis(5), AGAIN_FOR),
        send: Arc::new(move |sock: &str, request: &str, _wait: Duration| {
            let step = by_sock
                .lock()
                .unwrap()
                .get_mut(sock)
                .and_then(std::collections::VecDeque::pop_front);
            match step {
                Some((want, step)) if want == request => step(),
                Some((want, _)) => Leg::Failed(io::Error::other(format!(
                    "{sock}: sent {request:?}, the script wants {want:?}"
                ))),
                None => Leg::Failed(io::Error::other(format!(
                    "{sock}: nothing scripted for {request:?}"
                ))),
            }
        }),
    }
}

fn rows(rows: &[&str]) -> Vec<String> {
    rows.iter().map(|r| (*r).to_string()).collect()
}

fn timed_out(r: &[&str]) -> Reply {
    let r = rows(r);
    Box::new(move || Leg::TimedOut(r))
}

fn answered(r: &[&str]) -> Reply {
    let r = rows(r);
    Box::new(move || Leg::Answered(r))
}

fn refused(line: &'static str) -> Reply {
    Box::new(move || Leg::Failed(io::Error::other(line)))
}

fn step(f: impl FnOnce() -> Leg + Send + 'static) -> Reply {
    Box::new(f)
}

fn one_group(sock: &str, selectors: &[&str]) -> Vec<Group> {
    vec![Group {
        sock: sock.to_string(),
        targets: selectors
            .iter()
            .map(|s| Target {
                selector: (*s).to_string(),
                since: None,
                fact_since: None,
            })
            .collect(),
        order: Vec::new(),
        recorded: None,
    }]
}

fn waited(
    groups: Vec<Group>,
    combine: Combine,
    until: Option<Instant>,
    legs: &Legs,
) -> (Vec<String>, bool) {
    match wait_task_groups(groups, combine, false, until, &redial::Redial::none(), legs) {
        Ok(Waited::Rows { rows, answered }) => (rows, answered),
        Ok(Waited::Unanswered { why, .. }) => panic!("unanswered: {why}"),
        Ok(Waited::Code(code)) => panic!("an exit code: {code:?}"),
        Ok(Waited::Ungranted { why, .. }) => panic!("refused: {why}"),
        Err(e) => panic!("{e}"),
    }
}

/// The re-arm carries each target's fact edge as its baseline row read it (0 with no
/// `fact_rev=`); without `facts=1` no leg asks for facts and no element carries a fact edge.
#[test]
fn under_facts_1_the_rearm_carries_each_targets_fact_edge() {
    let x = "/x.sock";
    let legs = scripted(vec![
        (
            x,
            "await task @s-a,@s-b any facts=1 timeout=0\n",
            timed_out(&[
                "@s-a rev=2 state=working agent=- attention=- result_bytes=0",
                "@s-b rev=1 state=working agent=- attention=- result_bytes=0 fact=- fact_rev=4",
            ]),
        ),
        (
            x,
            "await task @s-a:2:0,@s-b:1:4 any facts=1 timeout=600\n",
            answered(&[
                "@s-a rev=2 state=working agent=- attention=- result_bytes=0 \
                 fact=hold-room@v1#1 fact_rev=1",
                "@s-b rev=1 state=working agent=- attention=- result_bytes=0 fact=- fact_rev=4",
            ]),
        ),
    ]);
    let groups = one_group(x, &["s-a", "s-b"]);
    let Ok(Waited::Rows {
        rows,
        answered: woke,
    }) = wait_task_groups(
        groups,
        Combine::Any,
        true,
        None,
        &redial::Redial::none(),
        &legs,
    )
    else {
        panic!("the scripted legs answer");
    };
    assert!(woke);
    assert!(
        rows[0].ends_with(" fact=hold-room@v1#1 fact_rev=1"),
        "{rows:?}"
    );
    let legs = scripted(vec![
        (
            x,
            "await task @s-b any timeout=0\n",
            timed_out(&[
                "@s-b rev=1 state=working agent=- attention=- result_bytes=0 fact=- fact_rev=4",
            ]),
        ),
        (
            x,
            "await task @s-b:1 any timeout=600\n",
            timed_out(&[
                "@s-b rev=1 state=working agent=- attention=- result_bytes=0 fact=hold-room@v1#1 fact_rev=5",
            ]),
        ),
        (
            x,
            "await task @s-b:1 any timeout=600\n",
            answered(&[
                "@s-b rev=2 state=done agent=- attention=- result_bytes=3 fact=- fact_rev=4",
            ]),
        ),
    ]);
    let (rows, woke) = waited(one_group(x, &["s-b"]), Combine::Any, None, &legs);
    assert!(woke);
    assert_eq!(rows.len(), 1);
}

#[test]
fn all_waits_do_not_latch_baseline_or_cleared_facts_or_display_attention() {
    let x = "/x.sock";
    let a = "@s-a rev=1 state=working agent=- attention=- result_bytes=0 fact=hold-room@v1#1 fact_rev=3";
    let b = "@s-b rev=1 state=working agent=- attention=- result_bytes=0";
    let final_a = "@s-a rev=3 state=done agent=- attention=- result_bytes=9";
    let final_b = "@s-b rev=2 state=done agent=- attention=- result_bytes=3";
    let legs = scripted(vec![
        (
            x,
            "await task @s-a,@s-b all facts=1 timeout=0\n",
            timed_out(&[a, b]),
        ),
        (
            x,
            "await task @s-a:1:3,@s-b:1:0 all facts=1 timeout=600\n",
            timed_out(&[
                "@s-a rev=2 state=working agent=- attention=an%20unrelated%20owner result_bytes=0 fact=hold-room@v1#2 fact_rev=4 task_news=0",
                b,
            ]),
        ),
        (
            x,
            "await task @s-a:1:3,@s-b:1:0 all facts=1 timeout=600\n",
            timed_out(&[
                "@s-a rev=2 state=done agent=- attention=- result_bytes=0 fact=- fact_rev=4 task_news=unknown",
                b,
            ]),
        ),
        (
            x,
            "await task @s-a:1:3,@s-b:1:0 all facts=1 timeout=600\n",
            answered(&[final_a, final_b]),
        ),
    ]);
    let result = wait_task_groups(
        one_group(x, &["s-a", "s-b"]),
        Combine::All,
        true,
        None,
        &redial::Redial::none(),
        &legs,
    );
    assert!(
        matches!(result, Ok(Waited::Rows { rows, answered: true }) if rows == [final_a, final_b])
    );
}

/// An all-wait keeps news it saw, even when a fact clears or a terminal task reopens before
/// the other target answers.
#[test]
fn all_waits_keep_terminal_news_that_came_and_went_between_legs() {
    all_waits_keep_news_between_legs(false, false);
}

#[test]
fn all_waits_keep_fact_news_that_came_and_went_between_legs() {
    all_waits_keep_news_between_legs(true, false);
}

#[test]
fn all_waits_keep_authoritative_attention_news_between_legs() {
    all_waits_keep_news_between_legs(false, true);
}

#[test]
fn all_waits_keep_news_the_initial_baseline_leg_already_latched() {
    let x = "/x.sock";
    let first_news = "@s-a rev=2 state=working agent=- attention=- result_bytes=0 task_news=1";
    let final_news = "@s-b rev=2 state=done agent=- attention=- result_bytes=3 task_news=1";
    let legs = scripted(vec![
        (
            x,
            "await task @s-a,@s-b all timeout=0\n",
            timed_out(&[
                first_news,
                "@s-b rev=1 state=working agent=- attention=- result_bytes=0 task_news=0",
            ]),
        ),
        (
            x,
            "await task @s-b:1 all timeout=600\n",
            answered(&[final_news]),
        ),
    ]);
    let result = wait_task_groups(
        one_group(x, &["s-a", "s-b"]),
        Combine::All,
        false,
        None,
        &redial::Redial::none(),
        &legs,
    );
    assert!(
        matches!(result, Ok(Waited::Rows { rows, answered: true }) if rows == [first_news, final_news])
    );
}

fn all_waits_keep_news_between_legs(facts: bool, attention: bool) {
    let x = "/x.sock";
    let baseline = "@s-a rev=1 state=working agent=- attention=- result_bytes=0";
    let other = "@s-b rev=1 state=working agent=- attention=- result_bytes=0";
    let news = if attention {
        "@s-a rev=2 state=working agent=- attention=- result_bytes=0 task_news=1"
    } else if facts {
        "@s-a rev=1 state=working agent=- attention=- result_bytes=0 fact=hold-room@v1#1 fact_rev=1"
    } else {
        "@s-a rev=2 state=done agent=- attention=- result_bytes=9"
    };
    let first = if facts {
        "await task @s-a,@s-b all facts=1 timeout=0\n"
    } else {
        "await task @s-a,@s-b all timeout=0\n"
    };
    let second = if facts {
        "await task @s-a:1:0,@s-b:1:0 all facts=1 timeout=600\n"
    } else {
        "await task @s-a:1,@s-b:1 all timeout=600\n"
    };
    let third = if facts {
        "await task @s-b:1:0 all facts=1 timeout=600\n"
    } else {
        "await task @s-b:1 all timeout=600\n"
    };
    let final_news = "@s-b rev=2 state=done agent=- attention=- result_bytes=3";
    let legs = scripted(vec![
        (x, first, timed_out(&[baseline, other])),
        (x, second, timed_out(&[news, other])),
        // s-a's news is held, so this leg does not ask for it again.
        (x, third, answered(&[final_news])),
    ]);
    let result = wait_task_groups(
        one_group(x, &["s-a", "s-b"]),
        Combine::All,
        facts,
        None,
        &redial::Redial::none(),
        &legs,
    );
    assert!(
        matches!(result, Ok(Waited::Rows { rows, answered: true }) if rows == [news, final_news]),
        "the all-wait must retain each observed row (facts={facts}, attention={attention})"
    );
}

#[test]
fn a_wait_rearms_past_its_baseline_and_keeps_a_worker_that_closed() {
    let x = "/x.sock";
    let legs = scripted(vec![
        (
            x,
            "await task @1,@s-b,@. all timeout=0\n",
            timed_out(&[
                "@s-a rev=1 state=working agent=- attention=- result_bytes=0",
                "@s-b rev=1 state=working agent=- attention=- result_bytes=0",
                "@s-c rev=1 state=working agent=- attention=- result_bytes=0",
            ]),
        ),
        (
            x,
            "await task @s-a:1,@s-b:1,@s-c:1 all timeout=600\n",
            timed_out(&[
                "@s-a rev=2 state=done agent=- attention=- result_bytes=9",
                "@s-b rev=1 state=working agent=busy attention=- result_bytes=0",
                "@s-c rev=1 state=working agent=- attention=- result_bytes=0",
            ]),
        ),
        (
            x,
            "await task @s-b:1,@s-c:1 all timeout=600\n",
            refused("ERR no such session @s-b"),
        ),
        (
            x,
            "await task @s-c:1 all timeout=600\n",
            answered(&["@s-c rev=3 state=exited agent=- attention=- result_bytes=0"]),
        ),
    ]);
    let (rows, answered) = waited(one_group(x, &["1", "s-b", "."]), Combine::All, None, &legs);
    assert!(answered);
    assert_eq!(
        rows,
        [
            "@s-a rev=2 state=done agent=- attention=- result_bytes=9",
            "@s-b rev=1 state=closed agent=busy attention=- result_bytes=0 task_state=working",
            "@s-c rev=3 state=exited agent=- attention=- result_bytes=0",
        ]
    );

    let legs = scripted(vec![
        (
            x,
            "await task @s-a,@s-b all timeout=0\n",
            timed_out(&[
                "@s-a rev=1 state=working agent=- attention=- result_bytes=0",
                "@s-b rev=4 state=closed agent=- attention=- result_bytes=0",
            ]),
        ),
        (
            x,
            "await task @s-a:1 all timeout=600\n",
            refused("ERR no such session @s-a"),
        ),
    ]);
    let (rows, answered) = waited(one_group(x, &["s-a", "s-b"]), Combine::All, None, &legs);
    assert!(answered);
    assert_eq!(
        rows,
        [
            "@s-a rev=1 state=closed agent=- attention=- result_bytes=0 task_state=working",
            "@s-b rev=4 state=closed agent=- attention=- result_bytes=0",
        ]
    );

    let legs = scripted(vec![(
        x,
        "await task @s-d all timeout=0\n",
        refused("ERR no such session @s-d"),
    )]);
    let refusal = wait_task_groups(
        one_group(x, &["s-d"]),
        Combine::All,
        false,
        None,
        &redial::Redial::none(),
        &legs,
    );
    assert!(
        matches!(&refusal, Err(e) if e.to_string() == "ERR no such session @s-d"),
        "the refusal ends the run"
    );
}

#[test]
fn a_leg_the_peer_did_not_serve_is_asked_again() {
    let x = "lab-mini";
    let busy = || step(|| Leg::Again(io::Error::other("lab-mini is busy")));
    let working = "@s-a rev=3 state=working agent=busy attention=- result_bytes=0";
    let done = "@s-a rev=4 state=done agent=idle attention=- result_bytes=9";
    let legs = scripted(vec![
        (x, "await task @s-a any timeout=0\n", busy()),
        (x, "await task @s-a any timeout=0\n", timed_out(&[working])),
        (x, "await task @s-a:3 any timeout=600\n", busy()),
        (x, "await task @s-a:3 any timeout=600\n", answered(&[done])),
    ]);
    let (rows, answered) = waited(one_group(x, &["s-a"]), Combine::Any, None, &legs);
    assert!(answered);
    assert_eq!(rows, [done]);

    let legs = scripted(vec![(
        x,
        "await task @s-a any timeout=0\n",
        refused("ERR denied: node ab12cd34 is not one the dialled Mac lets drive it"),
    )]);
    let refusal = wait_task_groups(
        one_group(x, &["s-a"]),
        Combine::Any,
        false,
        None,
        &redial::Redial::none(),
        &legs,
    );
    assert!(
        matches!(&refusal, Err(e) if e.to_string().starts_with("ERR denied")),
        "{:?}",
        refusal.err()
    );

    let legs = Legs {
        again: (Duration::ZERO, Duration::ZERO),
        ..scripted(vec![(x, "await task @s-a any timeout=0\n", busy())])
    };
    let gave_up = wait_task_groups(
        one_group(x, &["s-a"]),
        Combine::Any,
        false,
        None,
        &redial::Redial::none(),
        &legs,
    );
    assert!(
        matches!(&gave_up, Err(e) if e.to_string() == "lab-mini is busy"),
        "{:?}",
        gave_up.err()
    );
}

#[test]
fn instances_combine_any_and_all() {
    let (x, y) = ("/x.sock", "/y.sock");
    let two = || {
        let mut groups = one_group(x, &["s-a"]);
        groups.extend(one_group(y, &["s-b"]));
        groups
    };
    let legs = scripted(vec![
        (
            x,
            "await task @s-a all timeout=0\n",
            timed_out(&["@s-a rev=1 state=working agent=- attention=- result_bytes=0"]),
        ),
        (
            x,
            "await task @s-a:1 all timeout=600\n",
            answered(&["@s-a rev=2 state=done agent=- attention=- result_bytes=3"]),
        ),
        (
            y,
            "await task @s-b all timeout=0\n",
            timed_out(&["@s-b rev=4 state=working agent=- attention=- result_bytes=0"]),
        ),
        (
            y,
            "await task @s-b:4 all timeout=600\n",
            timed_out(&["@s-b rev=4 state=working agent=- attention=- result_bytes=0"]),
        ),
        (
            y,
            "await task @s-b:4 all timeout=600\n",
            answered(&["@s-b rev=5 state=stalled agent=- attention=- result_bytes=0"]),
        ),
    ]);
    assert_eq!(
        waited(two(), Combine::All, None, &legs),
        (
            rows(&[
                "@s-a rev=2 state=done agent=- attention=- result_bytes=3",
                "@s-b rev=5 state=stalled agent=- attention=- result_bytes=0",
            ]),
            true
        )
    );

    let (armed_tx, armed_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let legs = scripted(vec![
        (
            x,
            "await task @s-a any timeout=0\n",
            timed_out(&["@s-a rev=1 state=working agent=- attention=- result_bytes=0"]),
        ),
        (
            x,
            "await task @s-a:1 any timeout=600\n",
            step(move || {
                armed_rx
                    .recv_timeout(Duration::from_secs(60))
                    .expect("y parks");
                Leg::Answered(rows(&[
                    "@s-a rev=2 state=done agent=- attention=- result_bytes=3",
                ]))
            }),
        ),
        (
            y,
            "await task @s-b any timeout=0\n",
            timed_out(&["@s-b rev=4 state=working agent=- attention=- result_bytes=0"]),
        ),
        (
            y,
            "await task @s-b:4 any timeout=600\n",
            step(move || {
                let _ = armed_tx.send(());
                let _ = release_rx.recv();
                Leg::Failed(io::Error::other("released"))
            }),
        ),
    ]);
    assert_eq!(
        waited(two(), Combine::Any, None, &legs),
        (
            rows(&[
                "@s-a rev=2 state=done agent=- attention=- result_bytes=3",
                "@s-b rev=4 state=working agent=- attention=- result_bytes=0",
            ]),
            true
        )
    );
    drop(release_tx);

    let legs = scripted(vec![
        (
            x,
            "await task @s-a all timeout=0\n",
            timed_out(&["@s-a rev=1 state=working agent=- attention=- result_bytes=0"]),
        ),
        (
            y,
            "await task @s-b all timeout=0\n",
            timed_out(&["@s-b rev=4 state=working agent=- attention=- result_bytes=0"]),
        ),
    ]);
    let (rows, answered) = waited(two(), Combine::All, Some(Instant::now()), &legs);
    assert!(!answered);
    assert_eq!(rows.len(), 2, "{rows:?}");
}

#[test]
fn targets_group_by_the_instance_that_hosts_them() {
    let scratch = crate::test_scratch_dir("await-task");
    let dir = scratch.path();
    std::fs::create_dir_all(dir.join("graph")).expect("graph dir");
    let targets = vec![
        Target {
            selector: "s-abc".into(),
            since: None,
            fact_since: None,
        },
        Target {
            selector: "3".into(),
            since: None,
            fact_since: None,
        },
    ];
    let pinned = group(targets.clone(), true, "/tmp/pinned.sock", Some(dir));
    assert_eq!(pinned.len(), 1);
    assert_eq!(pinned[0].targets.len(), 2);
    // A sid no live instance hosts goes to the caller's own, which may keep the end of a
    // worker whose tab it closed, and otherwise refuses it.
    let unhosted = group(targets, false, "/tmp/default.sock", Some(dir));
    assert_eq!(unhosted.len(), 1);
    assert_eq!(unhosted[0].sock, "/tmp/default.sock");
    assert_eq!(unhosted[0].order, [0, 1]);
    let local = group(
        vec![Target {
            selector: ".".into(),
            since: None,
            fact_since: None,
        }],
        false,
        "/tmp/default.sock",
        Some(dir),
    );
    assert_eq!(local[0].sock, "/tmp/default.sock");
}

const BUSY: &str = "@s-a1 rev=3 state=working agent=busy attention=- result_bytes=0";
const NEEDS_YOU: &str = "@s-a1 rev=3 state=working agent=idle attention=needs-you \
                         result_bytes=0";

const HANDSHAKE: Duration = Duration::from_millis(30);

struct Peer {
    sent: Vec<u8>,
    deadline: Instant,
    reply: Option<io::Cursor<Vec<u8>>>,
}

// A driven Mac's `await task`: its timeout= runs from the request's arrival, a handshake
// after the dial, and the connection gives up at the dial's deadline.
impl io::Read for Peer {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.reply.is_none() {
            let sent = String::from_utf8_lossy(&self.sent).into_owned();
            let ms = sent
                .split_whitespace()
                .find_map(|word| word.strip_prefix("timeout="))
                .and_then(|ms| ms.parse().ok())
                .unwrap_or(0);
            let answers = Instant::now() + HANDSHAKE + Duration::from_millis(ms);
            if answers > self.deadline {
                std::thread::sleep(self.deadline.saturating_duration_since(Instant::now()));
                return Err(io::ErrorKind::WouldBlock.into());
            }
            std::thread::sleep(answers.saturating_duration_since(Instant::now()));
            let row = if ms == 0 { BUSY } else { NEEDS_YOU };
            self.reply = Some(io::Cursor::new(
                format!("OK 1 status=timeout\n{row}\n").into_bytes(),
            ));
        }
        self.reply.as_mut().map_or(Ok(0), |reply| reply.read(buf))
    }
}

impl io::Write for Peer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.sent.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn peer(_: &str, within: Duration) -> Result<crate::DriveLink, crate::DriveRefused> {
    Ok(crate::DriveLink {
        stream: Box::new(Peer {
            sent: Vec::new(),
            deadline: Instant::now() + within,
            reply: None,
        }),
        label: "lab-mini".to_owned(),
        node: None,
    })
}

struct Answers(io::Cursor<Vec<u8>>);

impl io::Read for Answers {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl io::Write for Answers {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn answers(line: &str) -> crate::DriveLink {
    crate::DriveLink {
        stream: Box::new(Answers(io::Cursor::new(format!("{line}\n").into_bytes()))),
        label: "lab-mini".to_owned(),
        node: None,
    }
}

#[test]
fn a_wait_its_own_timeout_ends_reads_the_peers_rows_of_its_last_leg() {
    let second = Some(Some(Duration::from_secs(1)));
    match driven_wait(&parts("await task @s-a1"), second, "lab-mini", peer) {
        Ok(Waited::Rows { rows, answered }) => {
            assert_eq!((rows, answered), (vec![NEEDS_YOU.to_owned()], false));
        }
        Ok(Waited::Unanswered { why, .. }) => panic!("unanswered: {why}"),
        Ok(Waited::Code(code)) => panic!("an exit code: {code:?}"),
        Ok(Waited::Ungranted { why, .. }) => panic!("refused: {why}"),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn a_handshake_a_later_leg_never_gets_keeps_its_cause_under_ctls_timeout() {
    static DIALS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    fn frozen_later(host: &str, within: Duration) -> Result<crate::DriveLink, crate::DriveRefused> {
        if DIALS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
            return peer(host, within);
        }
        std::thread::sleep(within);
        Err(crate::DriveRefused {
            unserved: true,
            ..crate::DriveRefused::once(format!(
                "127.0.0.1:7100: connected, but no fleet handshake within {:.1}s",
                within.as_secs_f64()
            ))
        })
    }
    let second = Some(Some(Duration::from_secs(1)));
    let refused = run_driven(&parts("await task @s-a1"), second, "lab-mini", frozen_later)
        .expect_err("unserved");
    assert_eq!(crate::exit_for(&refused), ExitCode::from(2), "{refused}");
    assert!(
        refused.to_string().contains("no fleet handshake"),
        "{refused}"
    );
}

const GONE: &str =
    "nothing listens on 127.0.0.1:7100: no aterm running there, or no [fleet] listen";

fn gone_after_baseline(baseline: &'static str, most: Duration) -> Legs {
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    Legs {
        max: Duration::from_millis(600),
        again: (Duration::from_millis(5), most),
        send: Arc::new(move |_: &str, _: &str, _: Duration| {
            if asked.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
                Leg::TimedOut(rows(&[baseline]))
            } else {
                Leg::Again(io::Error::other(GONE))
            }
        }),
    }
}

#[test]
fn a_peer_that_serves_no_leg_past_its_baseline_is_asked_until_the_timeout_and_named() {
    let working = "@s-a rev=3 state=working agent=busy attention=- result_bytes=0";
    for most in [Duration::from_millis(20), AGAIN_FOR] {
        let until = Instant::now() + Duration::from_millis(300);
        let ended = wait_task_groups(
            one_group("lab-mini", &["s-a"]),
            Combine::Any,
            false,
            Some(until),
            &redial::Redial::none(),
            &gone_after_baseline(working, most),
        );
        assert!(Instant::now() >= until, "never before its timeout");
        match ended {
            Ok(Waited::Unanswered { rows, why, .. }) => {
                assert_eq!(
                    (rows, why.to_string()),
                    (vec![working.to_owned()], GONE.into())
                );
            }
            Ok(Waited::Rows { rows, answered }) => {
                panic!("{rows:?} answered={answered}: the cause is dropped")
            }
            Ok(Waited::Code(code)) => panic!("an exit code: {code:?}"),
            Ok(Waited::Ungranted { why, .. }) => panic!("refused: {why}"),
            Err(e) => panic!("ended before its timeout: {e}"),
        }
    }
}

#[test]
fn a_wait_with_no_timeout_whose_peer_serves_no_leg_ends_a_minute_on_with_its_rows() {
    let working = "@s-a rev=3 state=working agent=busy attention=- result_bytes=0";
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let legs = Legs {
        max: Duration::from_millis(600),
        again: (Duration::from_millis(1), Duration::from_millis(20)),
        send: Arc::new(move |_: &str, _: &str, _: Duration| {
            match asked.fetch_add(1, std::sync::atomic::Ordering::Relaxed) {
                0 => Leg::TimedOut(rows(&[working])),
                // A hang detector: the bound on asking again is 20 asks or so.
                10_000.. => Leg::Code(ExitCode::from(99)),
                _ => Leg::Again(io::Error::other(GONE)),
            }
        }),
    };
    let ended = wait_task_groups(
        one_group("lab-mini", &["s-a"]),
        Combine::Any,
        false,
        None,
        &redial::Redial::none(),
        &legs,
    );
    match ended {
        Ok(Waited::Unanswered { rows, why, .. }) => {
            assert_eq!(
                (rows, why.to_string()),
                (vec![working.to_owned()], GONE.into())
            );
        }
        Ok(Waited::Rows { rows, answered }) => panic!("{rows:?} answered={answered}"),
        Ok(Waited::Code(code)) => panic!("asked again without bound: {code:?}"),
        Ok(Waited::Ungranted { why, .. }) => panic!("refused: {why}"),
        Err(e) => panic!("{e}"),
    }
}

const ASLEEP: &str = "lab-mini did not answer within 9.5s";

#[test]
fn a_driven_wait_whose_peer_went_away_or_fell_silent_ends_at_its_timeout_with_its_rows() {
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    fn later(
        dials: &AtomicUsize,
        (host, within): (&str, Duration),
        then: fn() -> Result<crate::DriveLink, crate::DriveRefused>,
    ) -> Result<crate::DriveLink, crate::DriveRefused> {
        if dials.fetch_add(1, Relaxed) == 0 {
            peer(host, within)
        } else {
            then()
        }
    }
    fn gone(host: &str, within: Duration) -> Result<crate::DriveLink, crate::DriveRefused> {
        static DIALS: AtomicUsize = AtomicUsize::new(0);
        later(&DIALS, (host, within), || {
            Err(crate::DriveRefused {
                again: true,
                ..crate::DriveRefused::once(GONE.to_owned())
            })
        })
    }
    fn asleep(host: &str, within: Duration) -> Result<crate::DriveLink, crate::DriveRefused> {
        static DIALS: AtomicUsize = AtomicUsize::new(0);
        later(&DIALS, (host, within), || {
            Err(crate::DriveRefused {
                silent: true,
                ..crate::DriveRefused::once(ASLEEP.to_owned())
            })
        })
    }
    fn frozen(host: &str, within: Duration) -> Result<crate::DriveLink, crate::DriveRefused> {
        static DIALS: AtomicUsize = AtomicUsize::new(0);
        later(&DIALS, (host, within), || {
            Ok(crate::DriveLink {
                stream: Box::new(Peer {
                    sent: Vec::new(),
                    deadline: Instant::now() + HANDSHAKE,
                    reply: None,
                }),
                label: "lab-mini".to_owned(),
                node: None,
            })
        })
    }
    for (drive, cause) in [
        (gone as crate::FleetDriveFn, GONE),
        (asleep, ASLEEP),
        (frozen, "lab-mini did not answer within "),
    ] {
        let began = Instant::now();
        let waited = driven_wait(
            &parts("await task @s-a1 timeout=1500"),
            None,
            "lab-mini",
            drive,
        );
        assert!(
            began.elapsed() >= Duration::from_millis(1500),
            "{cause}: never before its timeout"
        );
        match waited {
            Ok(Waited::Unanswered { rows, why, .. }) => {
                assert_eq!(rows, [BUSY], "{cause}: the row it read");
                assert!(why.to_string().starts_with(cause), "{why}");
            }
            Ok(Waited::Rows { rows, answered }) => {
                panic!("{cause}: {rows:?} answered={answered}, the cause dropped")
            }
            Ok(Waited::Code(code)) => panic!("{cause}: an exit code: {code:?}"),
            Ok(Waited::Ungranted { why, .. }) => panic!("refused: {why}"),
            Err(e) => panic!("{cause}: ended before its timeout: {e}"),
        }
    }
    let unanswered = Waited::Unanswered {
        at: "lab-mini".to_owned(),
        rows: Vec::new(),
        since: Instant::now(),
        why: io::Error::other(GONE),
        served: true,
    };
    assert_eq!(
        print_waited(unanswered, "lab-mini", crate::board::Writes::Nothing).unwrap(),
        ExitCode::from(redial::EXIT_REPLACED)
    );
}
#[test]
fn a_driven_wait_whose_peer_took_its_grant_back_ends_with_its_rows_and_says_so() {
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    const LOWERED: &str = "127.0.0.1:7100 lets no one drive it: its person runs `aterm fleet \
                           allow drive --from <this Mac>` there";
    const DENIED: &str = "node ab12cd34 is not one the dialled Mac lets drive it";
    fn lowered_now() -> Result<crate::DriveLink, crate::DriveRefused> {
        Err(crate::DriveRefused {
            ungranted: true,
            ..crate::DriveRefused::once(LOWERED.to_owned())
        })
    }
    fn lowered(host: &str, within: Duration) -> Result<crate::DriveLink, crate::DriveRefused> {
        static DIALS: AtomicUsize = AtomicUsize::new(0);
        if DIALS.fetch_add(1, Relaxed) == 0 {
            return peer(host, within);
        }
        lowered_now()
    }
    fn denied(host: &str, within: Duration) -> Result<crate::DriveLink, crate::DriveRefused> {
        static DIALS: AtomicUsize = AtomicUsize::new(0);
        if DIALS.fetch_add(1, Relaxed) == 0 {
            return peer(host, within);
        }
        Ok(answers(&format!(
            "{}{DENIED}",
            aterm_types::control_verbs::DRIVE_UNGRANTED
        )))
    }
    for (drive, was_lowered, says) in [
        (
            lowered as crate::FleetDriveFn,
            true,
            "its [fleet] allow is below drive now",
        ),
        (denied, false, DENIED),
    ] {
        let waited = driven_wait(
            &parts("await task @s-a1 timeout=15000"),
            None,
            "lab-mini",
            drive,
        );
        match waited {
            Ok(Waited::Ungranted {
                at,
                rows,
                why,
                lowered,
                served,
            }) => {
                assert_eq!(at, "lab-mini");
                assert_eq!((rows, lowered), (vec![BUSY.to_owned()], was_lowered));
                assert!(served, "the baseline was served");
                let said = ungranted(&at, &why, lowered, served);
                assert!(
                    said.starts_with(
                        "lab-mini served this wait's baseline and refuses this Mac's drive \
                         since (its person took back this Mac's grant"
                    ) && said.contains(says)
                        && !said.contains("--from <this Mac>"),
                    "{said}"
                );
            }
            Ok(Waited::Unanswered { why, .. }) => panic!("asked again: {why}"),
            Ok(Waited::Rows { rows, answered }) => panic!("{rows:?} answered={answered}"),
            Ok(Waited::Code(code)) => panic!("an exit code: {code:?}"),
            Err(e) => panic!("the rows it read are dropped: {e}"),
        }
    }
    fn lowered_first(_: &str, _: Duration) -> Result<crate::DriveLink, crate::DriveRefused> {
        lowered_now()
    }
    match driven_wait(&parts("await task @s-a1"), None, "lab-mini", lowered_first) {
        Err(e) => assert_eq!(e.to_string(), LOWERED, "before the baseline, its own words"),
        Ok(_) => panic!("a wait no leg of which was served"),
    }
    let refused = Waited::Ungranted {
        at: "lab-mini".to_owned(),
        rows: Vec::new(),
        why: io::Error::other(LOWERED),
        lowered: true,
        served: true,
    };
    assert_eq!(
        print_waited(refused, "lab-mini", crate::board::Writes::Nothing).unwrap(),
        ExitCode::FAILURE
    );
}

#[test]
fn a_driven_wait_past_its_baseline_closes_a_gone_worker_and_ends_any_other_denial_as_said() {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    const UNCHECKED: &str = "ERR denied: node ab12cd34 is bound on the dialled Mac only by \
                             its copy of its issuer's book, which is over 60 s old, and it \
                             could not read that book again to check it (timed out): it \
                             drives again once the dialled Mac reaches its issuer";
    const GATE: &str = "ERR denied: a drive names a session the driven Mac hosts by its sid \
                        (`survey --host` lists them)";
    static DIALS: AtomicUsize = AtomicUsize::new(0);
    static LATER: Mutex<&str> = Mutex::new("");
    fn then(host: &str, within: Duration) -> Result<crate::DriveLink, crate::DriveRefused> {
        if DIALS.fetch_add(1, Relaxed) == 0 {
            return peer(host, within);
        }
        Ok(answers(&LATER.lock().unwrap()))
    }
    let wait = |later: &'static str| {
        DIALS.store(0, Relaxed);
        *LATER.lock().unwrap() = later;
        driven_wait(
            &parts("await task @s-a1 timeout=15000"),
            None,
            "lab-mini",
            then,
        )
    };
    match wait("ERR no such session @s-a1") {
        Ok(Waited::Rows { rows, answered }) => assert_eq!(
            (rows, answered),
            (
                vec![format!(
                    "{} task_state=working",
                    BUSY.replace("state=working", "state=closed")
                )],
                true
            )
        ),
        Ok(Waited::Ungranted { why, .. }) => panic!("a gone worker read as a grant: {why}"),
        Ok(Waited::Unanswered { why, .. }) => panic!("unanswered: {why}"),
        Ok(Waited::Code(code)) => panic!("an exit code: {code:?}"),
        Err(e) => panic!("{e}"),
    }
    for denial in [UNCHECKED, GATE] {
        match wait(denial) {
            Err(e) => assert_eq!(e.to_string(), denial),
            Ok(Waited::Ungranted { why, .. }) => panic!("read as a grant taken back: {why}"),
            Ok(Waited::Rows { rows, answered }) => panic!("{rows:?} answered={answered}"),
            Ok(Waited::Unanswered { why, .. }) => panic!("unanswered: {why}"),
            Ok(Waited::Code(code)) => panic!("an exit code: {code:?}"),
        }
    }
}

#[test]
fn a_driven_wait_ctls_timeout_cuts_ends_as_unanswered_where_its_peer_froze_past_its_baseline() {
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    struct Frozen(Instant);
    impl io::Read for Frozen {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            std::thread::sleep(self.0.saturating_duration_since(Instant::now()));
            Err(io::ErrorKind::WouldBlock.into())
        }
    }
    impl io::Write for Frozen {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    fn freezes(
        dials: &AtomicUsize,
        (host, within): (&str, Duration),
    ) -> Result<crate::DriveLink, crate::DriveRefused> {
        if dials.fetch_add(1, Relaxed) == 0 {
            return peer(host, within);
        }
        Ok(crate::DriveLink {
            stream: Box::new(Frozen(Instant::now() + within)),
            label: "lab-mini".to_owned(),
            node: None,
        })
    }
    fn frozen(host: &str, within: Duration) -> Result<crate::DriveLink, crate::DriveRefused> {
        static DIALS: AtomicUsize = AtomicUsize::new(0);
        freezes(&DIALS, (host, within))
    }
    fn frozen_untimed(
        host: &str,
        within: Duration,
    ) -> Result<crate::DriveLink, crate::DriveRefused> {
        static DIALS: AtomicUsize = AtomicUsize::new(0);
        freezes(&DIALS, (host, within))
    }
    fn frozen_short(host: &str, within: Duration) -> Result<crate::DriveLink, crate::DriveRefused> {
        static DIALS: AtomicUsize = AtomicUsize::new(0);
        freezes(&DIALS, (host, within))
    }
    for (wait, drive, longer) in [
        (
            "await task @s-a1 timeout=60000",
            frozen as crate::FleetDriveFn,
            "a longer --timeout waits as long as it names, up to the wait's timeout= of 60000 \
             ms, and 0 that whole timeout=",
        ),
        (
            "await task @s-a1",
            frozen_untimed,
            "a longer --timeout waits as long as it names, and 0 until the news comes, where a \
             peer that answers no leg is asked again for a minute",
        ),
        // Its timeout= ran out inside the --timeout: a longer one waits out the leg alone, what
        // was left of its timeout= and 8 s.
        (
            "await task @s-a1 timeout=1000",
            frozen_short,
            "a longer --timeout, or 0, waits up to the drive's own 8",
        ),
    ] {
        let began = Instant::now();
        let flag = Some(Some(Duration::from_millis(1500)));
        let waited = driven_wait(&parts(wait), flag, "lab-mini", drive);
        assert!(
            began.elapsed() >= Duration::from_millis(1500),
            "never before its --timeout"
        );
        match waited {
            Ok(Waited::Unanswered { rows, why, .. }) => {
                assert_eq!(rows, [BUSY], "the row it read");
                assert!(
                    why.to_string().starts_with(&format!(
                        "no answer came from lab-mini within this call's --timeout of 1.5 s \
                         ({longer}"
                    )),
                    "{why}"
                );
            }
            Ok(Waited::Rows { rows, answered }) => {
                panic!("{rows:?} answered={answered}: a frozen peer is said as one that served")
            }
            Ok(Waited::Code(code)) => panic!("an exit code: {code:?}"),
            Ok(Waited::Ungranted { why, .. }) => panic!("refused: {why}"),
            Err(e) => panic!("ended before its timeout: {e}"),
        }
    }
}

/// NEGATIVE CONTROL: multi-instance `any` waits must still read every other group's baseline
/// row (and surface any baseline refusal), keep interleaved targets in caller order, and set
/// `task_news=1` when synthesizing a `state=closed` row from a baseline row that carried
/// `task_news=0`.
#[test]
fn multi_group_any_wait_keeps_interleaved_order_baseline_rows_and_closed_task_news() {
    let target = Target {
        selector: "s-a".into(),
        since: Some(1),
        fact_since: None,
    };
    assert_eq!(
        closed_row(
            Some(
                "@s-a rev=1 state=working agent=busy program_state=- attention=- result_bytes=0 task_news=0"
            ),
            &target,
        ),
        "@s-a rev=1 state=closed agent=busy program_state=- attention=- result_bytes=0 task_state=working task_news=1"
    );

    let (x, y) = ("/x.sock", "/y.sock");
    let interleaved = || {
        vec![
            Group {
                sock: x.to_string(),
                targets: vec![
                    Target {
                        selector: "s-a".into(),
                        since: Some(0),
                        fact_since: None,
                    },
                    Target {
                        selector: "s-c".into(),
                        since: Some(0),
                        fact_since: None,
                    },
                ],
                order: vec![0, 2],
                recorded: None,
            },
            Group {
                sock: y.to_string(),
                targets: vec![Target {
                    selector: "s-b".into(),
                    since: Some(0),
                    fact_since: None,
                }],
                order: vec![1],
                recorded: None,
            },
        ]
    };
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let legs = scripted(vec![
        (
            x,
            "await task @s-a:0,@s-c:0 any timeout=0\n",
            step(move || {
                let _ = done_tx.send(());
                Leg::Answered(rows(&[
                    "@s-a rev=2 state=done agent=- attention=- result_bytes=3 task_news=1",
                    "@s-c rev=1 state=working agent=- attention=- result_bytes=0 task_news=0",
                ]))
            }),
        ),
        (
            y,
            "await task @s-b:0 any timeout=0\n",
            step(move || {
                done_rx
                    .recv_timeout(Duration::from_secs(60))
                    .expect("x answered first");
                std::thread::sleep(Duration::from_millis(10));
                Leg::TimedOut(rows(&[
                    "@s-b rev=4 state=working agent=- attention=- result_bytes=0 task_news=0",
                ]))
            }),
        ),
    ]);
    let (got, woke) = waited(interleaved(), Combine::Any, None, &legs);
    assert!(woke);
    assert_eq!(
        got,
        rows(&[
            "@s-a rev=2 state=done agent=- attention=- result_bytes=3 task_news=1",
            "@s-b rev=4 state=working agent=- attention=- result_bytes=0 task_news=0",
            "@s-c rev=1 state=working agent=- attention=- result_bytes=0 task_news=0",
        ])
    );

    let (done_tx, done_rx) = mpsc::channel::<()>();
    let legs = scripted(vec![
        (
            x,
            "await task @s-a:0,@s-c:0 any timeout=0\n",
            step(move || {
                let _ = done_tx.send(());
                Leg::Answered(rows(&[
                    "@s-a rev=2 state=done agent=- attention=- result_bytes=3 task_news=1",
                    "@s-c rev=1 state=working agent=- attention=- result_bytes=0 task_news=0",
                ]))
            }),
        ),
        (
            y,
            "await task @s-b:0 any timeout=0\n",
            step(move || {
                done_rx
                    .recv_timeout(Duration::from_secs(60))
                    .expect("x answered first");
                std::thread::sleep(Duration::from_millis(10));
                Leg::Failed(io::Error::other("ERR no such session @s-b"))
            }),
        ),
    ]);
    let err = match wait_task_groups(
        interleaved(),
        Combine::Any,
        false,
        None,
        &redial::Redial::none(),
        &legs,
    ) {
        Err(err) => err,
        Ok(_) => panic!("baseline refusal on another group must not be dropped"),
    };
    assert_eq!(err.to_string(), "ERR no such session @s-b");
}

fn board_group(key: &str, pos: usize, sid: &str, rev: u64) -> Group {
    Group {
        sock: key.to_owned(),
        targets: vec![Target {
            selector: sid.to_owned(),
            since: Some(rev),
            fact_since: None,
        }],
        order: vec![pos],
        recorded: Some(vec![format!(
            "@{sid} rev={rev} state=working agent=- program_state=- attention=- \
             result_bytes=0 read=board"
        )]),
    }
}

/// A worker asked by its sid is answered only by a row naming that sid, in every wait: the
/// regression a driven Mac answering for another worker was, whose state then landed on that
/// worker's board row.
#[test]
fn a_row_naming_another_sid_is_no_answer_for_the_one_asked() {
    let legs = scripted(vec![(
        "drive:e",
        "await task @s-a1 any timeout=0\n",
        answered(&[
            "@s-b2 rev=9 state=closed agent=- attention=- result_bytes=0 \
                    conclusion=finished",
        ]),
    )]);
    match wait_task_groups(
        one_group("drive:e", &["s-a1"]),
        Combine::Any,
        false,
        None,
        &redial::Redial::none(),
        &legs,
    ) {
        Err(e) => assert!(e.to_string().contains("@s-b2"), "{e}"),
        Ok(_) => panic!("a row of another sid read as the answer for s-a1"),
    }
}

/// A Mac that refuses a board's wait before it answered at all says its rows are the
/// board's, never that it served a baseline it did not.
#[test]
fn a_board_wait_a_mac_refused_from_the_start_says_its_rows_are_the_boards() {
    let legs = scripted(vec![(
        "drive:m2",
        "await task @s-b2:1 any timeout=0\n",
        step(|| Leg::Ungranted {
            why: io::Error::other(format!(
                "{}node ab12cd34 is not one the dialled Mac lets drive it",
                aterm_types::control_verbs::DRIVE_UNGRANTED
            )),
            lowered: false,
        }),
    )]);
    let waited = wait_task_groups(
        vec![board_group("drive:m2", 0, "s-b2", 1)],
        Combine::Any,
        false,
        None,
        &redial::Redial::none(),
        &legs,
    );
    let (rows, why, lowered, served) = match waited {
        Ok(Waited::Ungranted {
            rows,
            why,
            lowered,
            served,
            ..
        }) => (rows, why, lowered, served),
        Ok(_) => panic!("a refusal before any answer read as another end"),
        Err(e) => panic!("a refusal before any answer ended the board's wait: {e}"),
    };
    assert!(!served);
    assert!(
        rows.iter().all(|row| row.ends_with(" read=board")),
        "{rows:?}"
    );
    let said = ungranted("m2", &why, lowered, served);
    assert!(
        said.contains("board's record") && !said.contains("baseline"),
        "{said}"
    );
    let said = unanswered("m2", Instant::now(), &io::Error::other("x"), false);
    assert!(
        said.contains("board's record") && !said.contains("last it read"),
        "{said}"
    );
}

/// A board's wait keeps each Mac's failure to its own rows: one Mac answering news ends an
/// `any` wait with the rows the board knows of a Mac that refused, and under `all` the wait
/// ends with every row, the refusing Mac's said, never an error for the whole wait.
#[test]
fn a_board_wait_keeps_one_macs_failure_to_its_rows() {
    let script = |m1: &'static str, m2: &'static str| {
        scripted(vec![
            (
                "drive:m1",
                m1,
                answered(
                    &["@s-a1 rev=4 state=done agent=- attention=- result_bytes=0 \
                           conclusion=finished"],
                ),
            ),
            (
                "drive:m2",
                m2,
                refused(
                    "ERR busy: an aterm window is open on this Mac and took no fleet \
                         connection (x); ask again",
                ),
            ),
        ])
    };
    let groups = || {
        vec![
            board_group("drive:m1", 0, "s-a1", 3),
            board_group("drive:m2", 1, "s-b2", 1),
        ]
    };
    let any = script(
        "await task @s-a1:3 any timeout=0\n",
        "await task @s-b2:1 any timeout=0\n",
    );
    let (rows, answered) = waited(groups(), Combine::Any, None, &any);
    assert!(answered);
    assert!(rows[0].contains("state=done"), "{rows:?}");
    assert!(rows[1].ends_with("read=board"), "{rows:?}");

    let all = wait_task_groups(
        groups(),
        Combine::All,
        false,
        None,
        &redial::Redial::none(),
        &script(
            "await task @s-a1:3 all timeout=0\n",
            "await task @s-b2:1 all timeout=0\n",
        ),
    )
    .expect("no error for the whole wait");
    let Waited::Unanswered { at, rows, .. } = all else {
        panic!("the refusing Mac is said");
    };
    assert_eq!(at, "drive:m2");
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows[0].contains("state=done"),
        "the Mac that answered: {rows:?}"
    );
    assert!(
        rows[1].ends_with("read=board"),
        "the Mac that refused: {rows:?}"
    );
}

/// A Mac with no window open hosts no worker: a board's rows on it close, and a row naming a
/// sid the leg did not ask for is no answer for the one it asked for.
#[test]
fn a_board_wait_closes_a_windowless_macs_workers_and_refuses_a_foreign_row() {
    let legs = scripted(vec![(
        "drive:m3",
        "await task @s-c3:2 any timeout=0\n",
        refused("ERR no window open on this Mac"),
    )]);
    let (rows, closed) = waited(
        vec![board_group("drive:m3", 0, "s-c3", 2)],
        Combine::Any,
        None,
        &legs,
    );
    assert!(closed, "a closed worker answers the wait");
    assert!(
        rows[0].starts_with("@s-c3 ") && rows[0].contains("state=closed"),
        "{rows:?}"
    );

    let legs = scripted(vec![(
        "drive:m4",
        "await task @s-d4:1 any timeout=0\n",
        answered(&[
            "@s-0ther rev=9 state=done agent=- attention=- result_bytes=0 \
                   conclusion=finished checked=0123456789ab@origin/main",
        ]),
    )]);
    let ended = wait_task_groups(
        vec![board_group("drive:m4", 0, "s-d4", 1)],
        Combine::Any,
        false,
        None,
        &redial::Redial::none(),
        &legs,
    )
    .expect("no error for the whole wait");
    let Waited::Unanswered { rows, why, .. } = ended else {
        panic!("a foreign row is no answer");
    };
    assert!(why.to_string().contains("malformed"), "{why}");
    assert_eq!(
        rows,
        [
            "@s-d4 rev=1 state=working agent=- program_state=- attention=- result_bytes=0 \
          read=board"
        ],
        "the board's own row, not the foreign one"
    );
}

/// A board's row is waited on where its worker runs: this Mac's own (`node=-` on its own
/// board, or its own node) at its instance, never over a drive to itself; another Mac's
/// over a drive to its node; `node=-` on another Mac's board over a drive to that Mac.
#[test]
fn a_board_row_is_waited_on_where_its_worker_runs() {
    let own = "0a".repeat(16);
    let other = "0b".repeat(16);
    let row = |node: Option<&str>| crate::board::Row {
        node: node.map(str::to_owned),
        ..crate::board::Row::started("s-0a1", 1)
    };
    let here = (Some("/here.sock"), Some(own.as_str()));
    let sock = Reach::Socket("/here.sock".to_owned());
    assert_eq!(reach_of(&row(None), BoardAt::Here, None, here), sock);
    assert_eq!(reach_of(&row(Some(&own)), BoardAt::Here, None, here), sock);
    assert_eq!(
        reach_of(&row(Some(&own)), BoardAt::Host("m14"), None, here),
        sock
    );
    assert_eq!(
        reach_of(&row(Some(&other)), BoardAt::Here, None, here),
        Reach::Drive(other.clone())
    );
    assert_eq!(
        reach_of(&row(None), BoardAt::Host("m14"), None, here),
        Reach::Drive("m14".to_owned())
    );
    assert!(matches!(
        reach_of(
            &row(Some(&own)),
            BoardAt::Here,
            None,
            (None, Some(own.as_str()))
        ),
        Reach::Nowhere(_)
    ));
}

/// With no `since=`, each worker's edge is its board row's revision; more than 64 workers on
/// one Mac go in several legs.
#[test]
fn a_board_wait_starts_each_worker_from_its_row_in_legs_of_64() {
    let rows: Vec<crate::board::Row> = (0..70u64)
        .map(|n| crate::board::Row {
            node: Some("0b".repeat(16)),
            rev: n,
            ..crate::board::Row::started(&format!("s-{:x}", 0x100 + n), n)
        })
        .collect();
    let (groups, reaches) = board_groups(&rows, BoardAt::Here, (None, None), (None, None));
    assert_eq!(reaches.len(), 1);
    assert_eq!(
        groups.iter().map(|g| g.targets.len()).collect::<Vec<_>>(),
        [64, 6]
    );
    assert_eq!(
        groups[1].targets[5].since,
        Some(69),
        "the row's own revision"
    );
    assert_eq!(groups[1].order[5], 69);
    let (groups, _) = board_groups(&rows, BoardAt::Here, (None, None), (Some(0), None));
    assert!(
        groups
            .iter()
            .flat_map(|g| &g.targets)
            .all(|t| t.since == Some(0)),
        "since= names every worker's edge"
    );
}

/// A WAIT ANOTHER ATERM SERVES IS VOUCHED FOR at the caller's own: one group at a sibling's socket
/// is served elsewhere. NEGATIVE CONTROL: every group at the caller's own socket is not, and no
/// vouch is asked.
#[test]
fn a_wait_a_sibling_serves_is_served_elsewhere() {
    let at = |sock: &str| Group {
        sock: sock.into(),
        targets: Vec::new(),
        order: Vec::new(),
        recorded: None,
    };
    let own = "/no/such/aterm-own.sock";
    assert!(!served_elsewhere(&[at(own)], own));
    assert!(!served_elsewhere(&[], own));
    assert!(served_elsewhere(
        &[at(own), at("/no/such/aterm-dev.sock")],
        own
    ));
}
