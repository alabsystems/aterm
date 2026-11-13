// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::wait::{MUST_HAPPEN, PARKED_SECS, until_within, within};

use super::*;

/// The wall clock at a case's first request in these tests (seconds since the epoch).
const UNIX_AT: f64 = 1_800_000_000.25;

fn request(input: Vec<J>) -> Option<J> {
    Some(J::obj([("model", J::s(MODEL)), ("input", J::Arr(input))]))
}

fn user() -> J {
    J::obj([
        ("type", J::s("message")),
        ("role", J::s("user")),
        (
            "content",
            J::Arr(vec![J::obj([
                ("type", J::s("input_text")),
                ("text", J::s(PROMPT)),
            ])]),
        ),
    ])
}

fn output() -> J {
    J::obj([
        ("type", J::s("function_call_output")),
        ("call_id", J::s(CALL_ID)),
        ("output", J::s("Plan updated")),
    ])
}

/// The requests of a recovered case: the overloaded answer's headers at
/// `header_start..header_at`, the retry's first byte at `retry_at`.
fn taken(header_start: f64, header_at: f64, retry_at: f64) -> Vec<Taken> {
    vec![
        Taken {
            arrived: 0.0,
            arrived_unix: UNIX_AT,
            body: request(vec![user()]),
            header_started: Some(0.01),
            header_sent: Some(0.011),
            resets_at: None,
        },
        Taken {
            arrived: 10.0,
            arrived_unix: UNIX_AT + 10.0,
            body: request(vec![user(), output()]),
            header_started: Some(header_start),
            header_sent: Some(header_at),
            resets_at: None,
        },
        Taken {
            arrived: retry_at,
            arrived_unix: UNIX_AT + retry_at,
            body: request(vec![user(), output()]),
            header_started: None,
            header_sent: None,
            resets_at: None,
        },
    ]
}

fn events(reply_type: &str, reply_event: &str) -> Vec<J> {
    let plan = J::obj([
        ("type", J::s("todo_list")),
        (
            "items",
            J::Arr(vec![J::obj([
                ("text", J::s(PLAN)),
                ("completed", J::Bool(true)),
            ])]),
        ),
    ]);
    vec![
        J::obj([("type", J::s("turn.started"))]),
        J::obj([("type", J::s("item.started")), ("item", plan.clone())]),
        J::obj([("type", J::s("item.completed")), ("item", plan)]),
        J::obj([
            ("type", J::s(reply_event)),
            (
                "item",
                J::obj([("type", J::s(reply_type)), ("text", J::s(ANSWER))]),
            ),
        ]),
        J::obj([("type", J::s("turn.completed"))]),
    ]
}

fn recovered(header_start: f64, header_at: f64, retry_at: f64) -> Result<J, String> {
    judge(
        Case::CapacityRecovery,
        Some(0),
        &taken(header_start, header_at, retry_at),
        &events("agent_message", "item.completed"),
    )
}

/// The smoke's own timing rows (`alab/tests/test_capacity_exec.py` at codex-alab
/// `767a333a16`, which held them while the smoke ran on its own): a retry two
/// seconds after the headers began passes, measured from the headers; one that request
/// handling before them would have excused, or a fifty-millisecond shortfall, is a bypass;
/// a bracket of header emission past the limit, backwards, or missing is no measure.
#[test]
fn the_retry_is_timed_from_the_overloaded_answers_headers() {
    let passed = recovered(13.0, 13.001, 15.001).expect("a retry after Retry-After");
    assert_eq!(passed.get("passed"), Some(&J::Bool(true)));
    let delay = passed.get("retryDelaySeconds").and_then(J::as_f64).unwrap();
    assert!((delay - 2.0).abs() < 1e-9, "{delay}");
    let overlap = recovered(13.0, 13.04, 15.0).expect("the actual bracket only");
    let delay = overlap
        .get("retryDelaySeconds")
        .and_then(J::as_f64)
        .unwrap();
    let emission = overlap
        .get("retryHeaderEmissionSeconds")
        .and_then(J::as_f64)
        .unwrap();
    assert!((delay - 1.96).abs() < 1e-9 && (emission - 0.04).abs() < 1e-9);
    for retry_at in [14.0, 14.96] {
        let refused = recovered(13.0, 13.001, retry_at).unwrap_err();
        assert!(
            refused.contains("Retry-After bypassed"),
            "{retry_at}: {refused}"
        );
    }
    assert!(
        recovered(13.0, 13.001, 15.0).is_ok(),
        "exactly at the deadline"
    );
    for (start, at) in [
        (13.0, 13.1),
        (13.1, 13.0),
        (13.0, f64::NAN),
        (13.0, f64::INFINITY),
    ] {
        assert!(recovered(start, at, 20.0).is_err(), "{start} {at}");
    }
    let mut missing = taken(13.0, 13.001, 20.0);
    missing[1].header_sent = None;
    let refused = judge(
        Case::CapacityRecovery,
        Some(0),
        &missing,
        &events("agent_message", "item.completed"),
    )
    .unwrap_err();
    assert!(refused.contains("missing or invalid"), "{refused}");
}

/// The smoke's verdict rows: the completed assistant reply passes; reasoning text
/// or a started reply does not stand in for it; a plan run twice, a retry that changed the
/// input, or a lost prompt fails; a refusal passes only with one request, one failed turn
/// and a non-zero exit, never a signal.
#[test]
fn the_verdict_holds_the_smokes_rows() {
    let good = events("agent_message", "item.completed");
    assert!(
        judge(
            Case::CapacityRecovery,
            Some(0),
            &taken(13.0, 13.001, 15.001),
            &good
        )
        .is_ok()
    );
    for (kind, event) in [
        ("reasoning", "item.completed"),
        ("agent_message", "item.started"),
    ] {
        let refused = judge(
            Case::CapacityRecovery,
            Some(0),
            &taken(13.0, 13.001, 15.001),
            &events(kind, event),
        )
        .unwrap_err();
        assert!(
            refused.contains("reply missing"),
            "{kind} {event}: {refused}"
        );
    }
    // As codex writes them and the parser reads them back: keys in the parser's order.
    let parsed: Vec<J> = [
        r#"{"type":"turn.started"}"#.to_string(),
        format!(
            r#"{{"type":"item.started","item":{{"id":"item_1","type":"todo_list","items":[{{"text":"{PLAN}","completed":true}}]}}}}"#
        ),
        format!(
            r#"{{"type":"item.completed","item":{{"id":"item_2","type":"agent_message","text":"{ANSWER}"}}}}"#
        ),
        format!(
            r#"{{"type":"item.completed","item":{{"id":"item_1","type":"todo_list","items":[{{"text":"{PLAN}","completed":true}}]}}}}"#
        ),
        r#"{"type":"turn.completed"}"#.to_string(),
    ]
    .iter()
    .map(|l| J::parse(l).unwrap())
    .collect();
    assert!(
        judge(
            Case::CapacityRecovery,
            Some(0),
            &taken(13.0, 13.001, 15.001),
            &parsed
        )
        .is_ok()
    );
    let mut twice = good.clone();
    twice.insert(3, good[2].clone());
    assert!(
        judge(
            Case::CapacityRecovery,
            Some(0),
            &taken(13.0, 13.001, 15.001),
            &twice
        )
        .unwrap_err()
        .contains("executed more than once")
    );
    let mut changed = taken(13.0, 13.001, 15.001);
    changed[2].body = request(vec![user()]);
    assert!(
        judge(Case::CapacityRecovery, Some(0), &changed, &good)
            .unwrap_err()
            .contains("retry changed")
    );
    let mut lost = taken(13.0, 13.001, 15.001);
    lost[0].body = request(Vec::new());
    assert!(
        judge(Case::CapacityRecovery, Some(0), &lost, &good)
            .unwrap_err()
            .contains("lost or duplicated")
    );
    assert!(
        judge(
            Case::CapacityRecovery,
            Some(1),
            &taken(13.0, 13.001, 15.001),
            &good
        )
        .unwrap_err()
        .contains("three requests")
    );

    let refusal = vec![
        J::obj([("type", J::s("turn.started"))]),
        J::obj([("type", J::s("turn.failed"))]),
    ];
    let one = &taken(13.0, 13.001, 15.001)[..1];
    let passed = judge(Case::PermanentRefusal, Some(1), one, &refusal).expect("a refusal");
    assert_eq!(passed.get("exit"), Some(&J::Int(1)));
    for exit in [None, Some(0)] {
        assert!(
            judge(Case::PermanentRefusal, exit, one, &refusal)
                .unwrap_err()
                .contains("the refusal was retried"),
            "{exit:?}"
        );
    }
    assert!(
        judge(
            Case::PermanentRefusal,
            Some(1),
            &taken(13.0, 13.001, 15.001)[..2],
            &refusal
        )
        .unwrap_err()
        .contains("the refusal was retried")
    );
}

/// The verdict rows the smoke's own test never reached (parity-6 review R7): a
/// request under another model; a refusal whose turn completed, or never failed; a plan
/// whose completed form changed. CONTROL: each case passes with the row's subject restored.
#[test]
fn the_verdict_holds_the_rows_the_smokes_test_left_out() {
    let good = events("agent_message", "item.completed");
    let mut other = taken(13.0, 13.001, 15.001);
    other[1].body = Some(J::obj([
        ("model", J::s("another-model")),
        ("input", J::Arr(vec![user(), output()])),
    ]));
    assert!(
        judge(Case::CapacityRecovery, Some(0), &other, &good)
            .unwrap_err()
            .contains("model changed")
    );
    assert!(
        judge(
            Case::CapacityRecovery,
            Some(0),
            &taken(13.0, 13.001, 15.001),
            &good
        )
        .is_ok()
    );

    let one = &taken(13.0, 13.001, 15.001)[..1];
    let turn = |t: &str| J::obj([("type", J::s(t))]);
    for (said, ends) in [
        ("completed", vec!["turn.completed"]),
        (
            "failed and completed",
            vec!["turn.failed", "turn.completed"],
        ),
        ("never failed", Vec::new()),
        ("failed twice", vec!["turn.failed", "turn.failed"]),
    ] {
        let mut refusal = vec![turn("turn.started")];
        refusal.extend(ends.into_iter().map(turn));
        assert!(
            judge(Case::PermanentRefusal, Some(1), one, &refusal)
                .unwrap_err()
                .contains("the refusal did not fail the native turn"),
            "{said}"
        );
    }
    assert!(
        judge(
            Case::PermanentRefusal,
            Some(1),
            one,
            &[turn("turn.started"), turn("turn.failed")]
        )
        .is_ok()
    );

    for (said, items) in [
        (
            "left incomplete",
            J::Arr(vec![J::obj([
                ("text", J::s(PLAN)),
                ("completed", J::Bool(false)),
            ])]),
        ),
        (
            "another step",
            J::Arr(vec![J::obj([
                ("text", J::s("another step")),
                ("completed", J::Bool(true)),
            ])]),
        ),
        ("no steps", J::Arr(Vec::new())),
    ] {
        let mut changed = good.clone();
        changed[2] = J::obj([
            ("type", J::s("item.completed")),
            (
                "item",
                J::obj([("type", J::s("todo_list")), ("items", items)]),
            ),
        ]);
        assert!(
            judge(
                Case::CapacityRecovery,
                Some(0),
                &taken(13.0, 13.001, 15.001),
                &changed
            )
            .unwrap_err()
            .contains("plan changed"),
            "{said}"
        );
    }
}

/// The model's answers, as `capacity_exec.py`'s model gives them: the plan call, the
/// overloaded answer with Retry-After, the reply, then a refusal of any extra request; a
/// refused case meets its wall at every request.
#[test]
fn the_lanes_model_answers_as_the_smokes_did() {
    let first = answer(Case::CapacityRecovery, 1, UNIX_AT);
    assert_eq!(
        (first.status, first.content_type, first.retry_after),
        (200, "text/event-stream", None)
    );
    assert!(first.body.contains(CALL_ID) && first.body.contains("update_plan"));
    let wall = answer(Case::CapacityRecovery, 2, UNIX_AT);
    assert_eq!((wall.status, wall.retry_after), (503, Some(2)));
    assert!(wall.body.contains("server_is_overloaded"));
    assert!(
        answer(Case::CapacityRecovery, 3, UNIX_AT)
            .body
            .contains(ANSWER)
    );
    assert_eq!(answer(Case::CapacityRecovery, 4, UNIX_AT).status, 400);
    assert_eq!(answer(Case::PermanentRefusal, 1, UNIX_AT).status, 403);
    assert_eq!(answer(Case::PermanentRefusal, 2, UNIX_AT).status, 403);
}

/// The lane's model for `case` on a loopback port of its own: the model, and a connection to it.
fn model_on_loopback(case: Case) -> (Arc<Model>, TcpStream) {
    let listener = aterm_uds::kept_out(|| TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    let model = Arc::new(Model {
        case,
        clock: Instant::now(),
        port,
        taken: Mutex::new(Vec::new()),
        witness: Arc::new(Witness::default()),
    });
    {
        let model = Arc::clone(&model);
        std::thread::spawn(move || serve(listener, model));
    }
    let conn = TcpStream::connect(("127.0.0.1", port)).unwrap();
    conn.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
    (model, conn)
}

/// One request to the model on `conn`, as codex asks: its answer's head and body.
fn exchange(conn: &mut TcpStream) -> (String, String) {
    let body = J::obj([("model", J::s(MODEL))]).dump(PY);
    write!(
        conn,
        "POST /v1/responses HTTP/1.1\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut r = std::io::BufReader::new(conn.try_clone().unwrap());
    let mut head = String::new();
    let mut length = 0;
    loop {
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        if line
            .get(..15)
            .is_some_and(|p| p.eq_ignore_ascii_case("content-length:"))
        {
            length = line[15..].trim().parse().unwrap();
        }
        head.push_str(&line);
        if line == "\r\n" {
            break;
        }
    }
    let mut rest = vec![0; length];
    std::io::Read::read_exact(&mut r, &mut rest).unwrap();
    (head, String::from_utf8(rest).unwrap())
}

/// The model stamps each answer's headers on that answer, between its request's arrival
/// and its body, over a real loopback connection.
#[test]
fn the_model_stamps_each_answers_headers_before_its_body() {
    let (model, mut conn) = model_on_loopback(Case::CapacityRecovery);
    for _ in 0..2 {
        exchange(&mut conn);
    }
    let taken = model.taken.lock().unwrap().clone();
    assert_eq!(taken.len(), 2);
    for t in &taken {
        let (start, sent) = (t.header_started.unwrap(), t.header_sent.unwrap());
        assert!(t.arrived <= start && start <= sent, "{t:?}");
    }
}

/// EACH WALL AS THE MODEL WRITES IT, over a real loopback connection: a terminal quota case's
/// answer is a `429 Too Many Requests` with `Retry-After` and its
/// `x-codex-rate-limit-reached-type`, and its body names the reset the model records on that
/// request, stamped on the wall clock no later than the reset's base; a reset case's `429`
/// names its reset and no type; a capacity wall names neither. CONTROL: the plan's answer
/// carries no wall header.
#[test]
fn the_model_writes_each_walls_headers_and_records_the_reset_it_named() {
    for (case, number) in [
        (Case::QuotaTerminal(TERMINAL_KINDS[2]), 1),
        (Case::QuotaResetRecovery, 2),
        (Case::CapacityRecovery, 2),
        (Case::QuotaResetRecovery, 1),
    ] {
        let (model, mut conn) = model_on_loopback(case);
        let mut said = (String::new(), String::new());
        for _ in 0..number {
            said = exchange(&mut conn);
        }
        let (head, body) = said;
        let taken = model.taken.lock().unwrap().clone();
        let last = taken.last().expect("the request taken");
        let named = J::parse(&body)
            .ok()
            .and_then(|j| j.get("error")?.get("resets_at")?.as_i64());
        let label = format!("{case:?} request {number}: {head}{body}");
        assert_eq!(last.resets_at, named, "{label}");
        assert!(
            last.arrived_unix.is_finite()
                && named.is_none_or(|reset| last.arrived_unix <= reset as f64),
            "{label}"
        );
        let has = |line: &str| head.lines().any(|l| l.eq_ignore_ascii_case(line));
        match (case, number) {
            (Case::QuotaTerminal(kind), _) => {
                assert!(
                    head.starts_with("HTTP/1.1 429 Too Many Requests\r\n"),
                    "{label}"
                );
                assert!(has("Retry-After: 2"), "{label}");
                assert!(
                    has(&format!("x-codex-rate-limit-reached-type: {kind}")),
                    "{label}"
                );
                assert!(named.is_some(), "{label}");
            }
            (Case::QuotaResetRecovery, 2) => {
                assert!(head.starts_with("HTTP/1.1 429 "), "{label}");
                assert!(has("Retry-After: 2"), "{label}");
                assert!(named.is_some(), "{label}");
                assert!(!head.contains("x-codex-rate-limit-reached-type"), "{label}");
            }
            (Case::CapacityRecovery, _) => {
                assert!(head.starts_with("HTTP/1.1 503 "), "{label}");
                assert!(has("Retry-After: 2"), "{label}");
                assert!(named.is_none(), "{label}");
                assert!(!head.contains("x-codex-rate-limit-reached-type"), "{label}");
            }
            _ => {
                assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{label}");
                assert!(
                    !head.to_ascii_lowercase().contains("retry-after"),
                    "{label}"
                );
                assert!(!head.contains("x-codex-rate-limit-reached-type"), "{label}");
            }
        }
    }
}

/// EACH QUOTA WALL AS THE LANE'S MODEL ANSWERS IT: `usage_limit_reached`, with
/// `Retry-After: 2`; a reset case names the moment it was asked, rounded up to the second,
/// 3 s on (`Retry-After` and [`RESET_PAST_RETRY_S`]), and no type, so asked at any fraction of
/// a second its reset is a second or more past `Retry-After` (a retry that keeps
/// `Retry-After` alone, less than a second late, still comes before it) and within what the case
/// waits by design; a no-reset case names no reset; a `403` case meets its wall at every
/// request, with or without its reset; a terminal case meets a `429` typed with its kind at
/// every request, naming its reset. The case table holds ten cases of distinct names, every
/// terminal kind once among them, and seven refusals.
#[test]
fn the_lanes_model_answers_each_quota_wall_past_what_retry_after_alone_reaches() {
    let error = |a: &Answer| J::parse(&a.body).expect("JSON").get("error").cloned();
    let reset_of = |a: &Answer| error(a).and_then(|e| e.get("resets_at")?.as_i64());
    let typed = |a: &Answer| {
        error(a)
            .and_then(|e| e.get("type")?.as_str().map(str::to_string))
            .unwrap_or_default()
    };

    let wall = answer(Case::QuotaResetRecovery, 2, UNIX_AT);
    assert_eq!((wall.status, wall.retry_after), (429, Some(2)));
    assert_eq!(typed(&wall), "usage_limit_reached");
    assert_eq!(wall.resets_at, Some(1_800_000_004), "rounded up, 3 s on");
    assert_eq!(reset_of(&wall), wall.resets_at, "the body names it");
    assert_eq!(wall.reached_type, None);
    let on_the_second = answer(Case::QuotaResetRecovery, 2, 1_800_000_000.0);
    assert_eq!(on_the_second.resets_at, Some(1_800_000_003));
    let retry_after = RETRY_AFTER_S as f64;
    let waits = Case::QuotaResetRecovery.waits().as_secs_f64();
    for fraction in [0.0, 0.001, 0.25, 0.5, 0.997, 0.999_999] {
        let asked = 1_800_000_000.0 + fraction;
        let reset = answer(Case::QuotaResetRecovery, 2, asked)
            .resets_at
            .expect("a reset") as f64;
        assert!(
            reset >= asked + retry_after + 1.0,
            "asked at {asked}: a retry under a second past Retry-After reaches the reset {reset}"
        );
        assert!(
            reset <= asked + waits,
            "asked at {asked}: the reset {reset} past the case's wait {waits}"
        );
    }
    assert_eq!(answer(Case::QuotaResetRecovery, 1, UNIX_AT).status, 200);
    assert!(
        answer(Case::QuotaResetRecovery, 3, UNIX_AT)
            .body
            .contains(ANSWER)
    );

    let unnamed = answer(Case::QuotaNoResetRecovery, 2, UNIX_AT);
    assert_eq!((unnamed.status, unnamed.retry_after), (429, Some(2)));
    assert_eq!(typed(&unnamed), "usage_limit_reached");
    assert_eq!((unnamed.resets_at, reset_of(&unnamed)), (None, None));
    assert_eq!(answer(Case::QuotaNoResetRecovery, 3, UNIX_AT).status, 200);

    for (case, reset) in [
        (Case::QuotaPermanentRefusal, Some(1_800_000_004)),
        (Case::QuotaNoResetRefusal, None),
    ] {
        for number in [1, 2] {
            let a = answer(case, number, UNIX_AT);
            assert_eq!((a.status, a.retry_after), (403, Some(2)), "{case:?}");
            assert_eq!(typed(&a), "usage_limit_reached", "{case:?}");
            assert_eq!((a.resets_at, reset_of(&a)), (reset, reset), "{case:?}");
            assert_eq!(a.reached_type, None, "{case:?}");
        }
    }
    for kind in TERMINAL_KINDS {
        for number in [1, 2] {
            let a = answer(Case::QuotaTerminal(kind), number, UNIX_AT);
            assert_eq!((a.status, a.retry_after), (429, Some(2)), "{kind}");
            assert_eq!(typed(&a), "usage_limit_reached", "{kind}");
            assert_eq!(a.reached_type, Some(kind));
            assert_eq!(reset_of(&a), Some(1_800_000_004), "{kind}: its reset");
        }
    }
    for case in [Case::CapacityRecovery, Case::PermanentRefusal] {
        let a = answer(case, 2, UNIX_AT);
        assert_eq!((a.resets_at, a.reached_type), (None, None), "{case:?}");
        assert!(a.body.contains("server_is_overloaded"), "{case:?}");
    }

    let names: std::collections::BTreeSet<String> = CASES.iter().map(|c| c.name()).collect();
    assert_eq!(names.len(), CASES.len(), "{names:?}");
    for kind in TERMINAL_KINDS {
        assert_eq!(
            CASES
                .iter()
                .filter(|c| **c == Case::QuotaTerminal(kind))
                .count(),
            1,
            "{kind}"
        );
    }
    assert_eq!(CASES.iter().filter(|c| c.refused()).count(), 7);
}

/// The requests of a recovered quota case: the wall's headers at `13.0..13.001` on the
/// lane's clock, its answer naming `reset`, the retry's first byte at `retry_at` (lane clock)
/// and `retry_unix` (wall clock).
fn quota_taken(reset: Option<i64>, retry_at: f64, retry_unix: f64) -> Vec<Taken> {
    let mut t = taken(13.0, 13.001, retry_at);
    t[1].resets_at = reset;
    t[2].arrived_unix = retry_unix;
    t
}

fn judged(case: Case, t: &[Taken]) -> Result<J, String> {
    judge(case, Some(0), t, &events("agent_message", "item.completed"))
}

/// THE QUOTA'S RESET (`capacity_exec.py --quota`): a reset case's retry on the wall clock at
/// or past the reset its wall named passes, and records both; one before it fails though
/// `Retry-After` was kept, and so does a wall that named none, or a retry stamp that is not
/// finite. CONTROL: a capacity case of the same requests passes, the reset not its row.
#[test]
fn the_quota_verdict_holds_the_reset() {
    const RESET: i64 = 1_800_000_016;
    let reset = RESET as f64;
    let kept = judged(
        Case::QuotaResetRecovery,
        &quota_taken(Some(RESET), 16.0, reset + 0.001),
    )
    .expect("a retry past the reset");
    assert_eq!(kept.get("quotaResetUnix"), Some(&J::Int(RESET)));
    assert_eq!(
        kept.get("retryUnix").and_then(J::as_f64),
        Some(reset + 0.001)
    );
    assert!(
        judged(
            Case::QuotaResetRecovery,
            &quota_taken(Some(RESET), 16.0, reset)
        )
        .is_ok(),
        "exactly at the reset"
    );
    let early = quota_taken(Some(RESET), 16.0, reset - 0.5);
    let refused = judged(Case::QuotaResetRecovery, &early).unwrap_err();
    assert!(refused.contains("retried before its reset"), "{refused}");
    assert!(judged(Case::CapacityRecovery, &early).is_ok(), "control");
    let refused = judged(
        Case::QuotaResetRecovery,
        &quota_taken(None, 16.0, reset + 1.0),
    )
    .unwrap_err();
    assert!(refused.contains("named no reset"), "{refused}");
    for unix in [f64::NAN, f64::INFINITY] {
        let refused = judged(
            Case::QuotaResetRecovery,
            &quota_taken(Some(RESET), 16.0, unix),
        )
        .unwrap_err();
        assert!(refused.contains("missing or invalid"), "{unix}: {refused}");
    }
}

/// THE POLL PAST A QUOTA WALL WITH NO RESET (`capacity_exec.py --quota-no-reset`): a retry 30 s
/// or more after the wall's headers were written passes, and records the poll; one sooner
/// fails though `Retry-After` was kept, and so do missing or non-finite stamps. CONTROL: a
/// capacity case of the same requests passes, the poll not its row.
#[test]
fn the_quota_verdict_holds_the_poll() {
    let polled = judged(
        Case::QuotaNoResetRecovery,
        &quota_taken(None, 43.001, UNIX_AT + 43.001),
    )
    .expect("a poll 30 s on");
    let poll = polled.get("pollDelaySeconds").and_then(J::as_f64).unwrap();
    assert!((poll - 30.0).abs() < 1e-9, "{poll}");
    for retry_at in [15.001, 43.0] {
        let soon = quota_taken(None, retry_at, UNIX_AT + retry_at);
        let refused = judged(Case::QuotaNoResetRecovery, &soon).unwrap_err();
        assert!(
            refused.contains("bypassed its polling backoff"),
            "{retry_at}: {refused}"
        );
        assert!(judged(Case::CapacityRecovery, &soon).is_ok(), "control");
    }
    let mut unstamped = quota_taken(None, 50.0, UNIX_AT + 50.0);
    unstamped[1].header_sent = None;
    assert!(
        quota_poll_kept(&unstamped[1], &unstamped[2])
            .unwrap_err()
            .contains("missing or invalid")
    );
    for at in [f64::NAN, f64::INFINITY] {
        let mut t = quota_taken(None, 50.0, UNIX_AT + 50.0);
        t[2].arrived = at;
        assert!(
            quota_poll_kept(&t[1], &t[2])
                .unwrap_err()
                .contains("missing or invalid"),
            "{at}"
        );
    }
}

/// A package's bytes decide: words an older package also holds do not count. CONTROL: a package
/// that holds the recovery is judged on every case, so it still fails if it does not recover.
#[test]
fn a_package_with_no_quota_recovery_skips_only_the_cases_that_need_it() {
    assert!(holds_quota_recovery(b"a Waiting for account quota b"));
    assert!(
        !holds_quota_recovery(b"usage_limit_reached resets_at"),
        "an older package parses the quota body too; that is not the recovery"
    );
    let names = |holds| cases_for(holds).map(Case::name).collect::<Vec<_>>();
    assert_eq!(
        names(true).len(),
        CASES.len(),
        "CONTROL: it is judged on all"
    );
    let without = names(false);
    assert_eq!(without.len(), CASES.len() - 2, "{without:?}");
    assert!(
        without.contains(&"capacity-recovery".to_owned()),
        "{without:?}"
    );
    assert!(!without.contains(&"quota-reset-recovery".to_owned()));
    assert!(!without.contains(&"quota-no-reset-recovery".to_owned()));
}

/// EVERY REFUSAL'S ROWS AND EVERY RECOVERY'S, case by case: a refused case passes with one
/// request, one failed turn and a non-zero exit, and fails retried, signaled, exiting 0, or
/// with its turn completed or never failed; a recovering case fails a non-zero exit or a
/// request short. CONTROL: each recovering case passes the rows its wall does not add.
#[test]
fn every_case_holds_its_refusal_or_recovery_rows() {
    let turn = |t: &str| J::obj([("type", J::s(t))]);
    let failed = [turn("turn.started"), turn("turn.failed")];
    let ok = taken(13.0, 13.001, 15.001);
    for case in CASES.into_iter().filter(|c| c.refused()) {
        let one = &ok[..1];
        let passed = judge(case, Some(1), one, &failed).expect("a refusal");
        assert_eq!(passed.get("case"), Some(&J::s(case.name())));
        for exit in [None, Some(0)] {
            assert!(
                judge(case, exit, one, &failed)
                    .unwrap_err()
                    .contains("the refusal was retried"),
                "{case:?} {exit:?}"
            );
        }
        assert!(
            judge(case, Some(1), &ok[..2], &failed)
                .unwrap_err()
                .contains("the refusal was retried"),
            "{case:?}"
        );
        for ends in [vec!["turn.completed"], Vec::new()] {
            let mut events = vec![turn("turn.started")];
            events.extend(ends.into_iter().map(turn));
            assert!(
                judge(case, Some(1), one, &events)
                    .unwrap_err()
                    .contains("the refusal did not fail the native turn"),
                "{case:?}"
            );
        }
    }
    let good = events("agent_message", "item.completed");
    for case in CASES.into_iter().filter(|c| !c.refused()) {
        assert!(
            judge(case, Some(1), &ok, &good)
                .unwrap_err()
                .contains("did not recover in three requests"),
            "{case:?}"
        );
        assert!(
            judge(case, Some(0), &ok[..2], &good)
                .unwrap_err()
                .contains("did not recover in three requests"),
            "{case:?}"
        );
    }
    assert!(judge(Case::CapacityRecovery, Some(0), &ok, &good).is_ok());
}

/// The `sleep` a run left: the pid its script wrote to `root/left.pid`.
fn left(root: &Path) -> i32 {
    crate::util::read_regular_text(&root.join("left.pid"))
        .expect("the run wrote its sleep's pid")
        .trim()
        .parse()
        .expect("a pid")
}

/// Wait until `pid` ended: gone, or a zombie its new parent has yet to reap.
fn ends(pid: i32, at: &str) {
    // Each look runs `ps`: 50 ms apart, not `until_within`'s 10.
    let ended = within(MUST_HAPPEN, Duration::from_millis(50), || {
        crate::procs::state_of(pid)
            .is_none_or(|stat| stat.starts_with('Z'))
            .then_some(())
    });
    assert!(
        ended.is_some(),
        "{at}: the sleep it left ({pid}) outlived the run"
    );
}

/// A run in `root` that leads a process group of its own and waits on a `sleep` it started
/// in it (living [`PARKED_SECS`]), once that `sleep`'s pid is in `root/left.pid`. Neither
/// holds this process's stdout or stderr, so a probe's parent reads the probe's end when the
/// probe ends, not when what it left does.
fn waiting_run(root: &Path) -> aterm_uds::spawnfd::Child {
    let child = aterm_uds::spawnfd::command("/bin/sh")
        .args([
            "-c",
            &format!(
                "/bin/sleep {PARKED_SECS} & echo $! > left.new && /bin/mv left.new left.pid; wait"
            ),
        ])
        .current_dir(root)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the run starts");
    until_within(MUST_HAPPEN, "the run's start of its sleep", || {
        root.join("left.pid").exists().then_some(())
    });
    child
}

/// THE RUN'S GROUP ENDS WITH IT: a `sleep` a run left in its group is killed once the run
/// exits (through [`Jail::run`] itself: `sandbox-exec`, a profile that fences nothing), and
/// once a run that waits on it reaches its bound (through [`end_with_group`], called with
/// the bound already past once the `sleep` has started), which reads as no exit. CONTROL:
/// with only the leader killed at the bound, as before, the `sleep` outlives the run in
/// both cases and the wait for its end runs out.
#[test]
fn what_a_run_leaves_in_its_group_ends_at_its_exit_and_at_its_bound() {
    let sleep = format!("/bin/sleep {PARKED_SECS}");

    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let root = tmp.path();
    let env = fresh_environment(root).expect("the run's scratch");
    let witness = Witness::default();
    let ran = Jail {
        root,
        env: &env,
        profile: "(version 1)(allow default)",
        witness: &witness,
    }
    .run(
        Path::new("/bin/sh"),
        &["-c", &format!("{sleep} & echo $! > ../left.pid")],
        "group",
        RUN_LIMIT,
    )
    .expect("the run starts");
    assert_eq!(ran, Some(0), "the run's own exit");
    ends(left(root), "at the run's exit");

    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let root = tmp.path();
    let mut child = waiting_run(root);
    let ran =
        end_with_group(&mut child, &Witness::default(), Duration::ZERO).expect("the run is reaped");
    assert_eq!(ran, None, "a run stopped at its bound has no exit");
    ends(left(root), "at the run's bound");
}

const VERSION: &str = "0.1.0-dev.1+codex.0.166.0.gabcdef012";
const SOURCE: &str = "0123456789abcdef0123456789abcdef01234567";
const TARGET: &str = "aarch64-apple-darwin";

/// A package in a fresh scratch dir with `build` as its `alab-build.json`, `manifest` as its
/// `codex-package.json` (each a JSON object of strings) and a regular `bin/codex`.
fn records(build: [(&str, &str); 3], manifest: [(&str, &str); 3]) -> aterm_tempfile::TempDir {
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("bin")).expect("its bin");
    let obj = |fields: [(&str, &str); 3]| J::obj(fields.map(|(k, v)| (k, J::s(v)))).dump(PY);
    crate::util::write_fresh(&root.join("alab-build.json"), obj(build)).expect("its build");
    crate::util::write_fresh(&root.join("codex-package.json"), obj(manifest))
        .expect("its manifest");
    crate::util::write_fresh(&root.join("bin/codex"), "#!/bin/sh\n").expect("its codex");
    tmp
}

/// A package whose records agree.
fn agreed() -> aterm_tempfile::TempDir {
    records(
        [("version", VERSION), ("target", TARGET), ("source", SOURCE)],
        [
            ("version", VERSION),
            ("target", TARGET),
            ("entrypoint", "bin/codex"),
        ],
    )
}

/// THE PACKAGE'S RECORDS (review F1): records that agree give the version, the exact source
/// and the entrypoint inside the package; a version or target that disagrees, a source that
/// is not 40 lowercase hex, another entrypoint, a `bin/codex` that is missing, a directory or
/// a link out of the package, and a record that is missing, a link, oversized or not JSON
/// are each refused, by name.
#[test]
fn the_package_records_agree_and_name_an_entrypoint_inside_the_package() {
    let refused = |pkg: &aterm_tempfile::TempDir| package_records(pkg.path()).expect_err("refused");
    let good = agreed();
    let (version, source, exe) = package_records(good.path()).expect("records that agree");
    assert_eq!((version.as_str(), source.as_str()), (VERSION, SOURCE));
    assert_eq!(
        exe,
        std::fs::canonicalize(good.path().join("bin/codex")).expect("its codex")
    );

    let pkg = |build_version, target, source, entry| {
        records(
            [
                ("version", build_version),
                ("target", target),
                ("source", source),
            ],
            [
                ("version", VERSION),
                ("target", TARGET),
                ("entrypoint", entry),
            ],
        )
    };
    let other = pkg("0.1.0-dev.2", TARGET, SOURCE, "bin/codex");
    assert_eq!(refused(&other), "package versions disagree");
    let other = pkg(VERSION, "x86_64-apple-darwin", SOURCE, "bin/codex");
    assert_eq!(refused(&other), "package targets disagree");
    let (longer, upper, not_hex) = (
        format!("{SOURCE}0"),
        SOURCE.to_ascii_uppercase(),
        SOURCE.replace('a', "g"),
    );
    let sources: [&str; 5] = [&SOURCE[..39], &longer, &upper, &not_hex, ""];
    for source in sources {
        let other = pkg(VERSION, TARGET, source, "bin/codex");
        assert_eq!(refused(&other), "missing exact source commit", "{source:?}");
    }
    let other = pkg(VERSION, TARGET, SOURCE, "bin/other");
    assert_eq!(refused(&other), "unexpected package entrypoint");

    let outside = aterm_tempfile::tempdir().expect("a scratch dir");
    let elsewhere = outside.path().join("codex");
    crate::util::write_fresh(&elsewhere, "#!/bin/sh\n").expect("a codex elsewhere");
    let linked = agreed();
    std::fs::remove_file(linked.path().join("bin/codex")).expect("its codex");
    std::os::unix::fs::symlink(&elsewhere, linked.path().join("bin/codex")).expect("a link out");
    assert_eq!(refused(&linked), "entrypoint escapes package");
    let missing = agreed();
    std::fs::remove_file(missing.path().join("bin/codex")).expect("its codex");
    assert_eq!(refused(&missing), "entrypoint escapes package");
    std::fs::create_dir(missing.path().join("bin/codex")).expect("a directory");
    assert_eq!(refused(&missing), "entrypoint escapes package");

    let gone = agreed();
    std::fs::remove_file(gone.path().join("alab-build.json")).expect("its build");
    assert_eq!(refused(&gone), "missing regular alab-build.json");
    let link = agreed();
    let manifest = link.path().join("codex-package.json");
    std::fs::rename(&manifest, link.path().join("real.json")).expect("moved");
    std::os::unix::fs::symlink("real.json", &manifest).expect("a link");
    assert_eq!(refused(&link), "missing regular codex-package.json");
    let big = agreed();
    crate::util::write_fresh(
        &big.path().join("alab-build.json"),
        vec![b' '; 1024 * 1024 + 1],
    )
    .expect("oversized");
    assert_eq!(refused(&big), "oversized alab-build.json");
    let torn = agreed();
    crate::util::write_fresh(&torn.path().join("alab-build.json"), "{").expect("torn");
    assert!(
        refused(&torn).starts_with("alab-build.json: "),
        "{}",
        refused(&torn)
    );
}

/// The witness's view of a run of the tree `pids`: two samples on the clock that saw no
/// socket, then one at each request to the model on port 5000, each seeing `lsof` (that
/// request's `lsof -F pcPtn` output).
fn seen_at(lsof: &[&str], pids: &[i32]) -> Seen {
    let mut seen = Seen::default();
    for _ in 0..2 {
        seen.record(pids, "", None);
    }
    for each in lsof {
        seen.record(pids, each, Some(5000));
    }
    seen
}

/// The same, `at_requests` requests each sampled seeing `lsof`.
fn seen(lsof: &str, pids: &[i32], at_requests: usize) -> Seen {
    seen_at(&vec![lsof; at_requests], pids)
}

/// THE EGRESS VERDICT (review F1): a tree seen only on loopback, sampled at each of the
/// model's requests, its connection to the model seen at each and the canary untouched,
/// holds; a socket off loopback, a request the witness did not sample at or whose sample did
/// not see its connection (review R2: one connection seen for three requests was enough), a
/// run with no request at all, a connection to another port or by a pid outside the tree,
/// and a touched canary each fail it.
#[test]
fn the_egress_verdict_holds_a_loopback_tree_seen_at_every_request_alone() {
    let conn = "p4100\nccodex\nPTCP\nn127.0.0.1:61000->127.0.0.1:5000\n";
    let good = seen(conn, &[4100], 3);
    assert!(good.egress_held(5000, 3, false));
    assert_eq!(good.log(), "codex[4100] TCP conn 127.0.0.1:5000\n");
    assert_eq!((good.samples, good.at_requests), (5, 3));
    let listening = "p4100\nccodex\nPTCP\nn127.0.0.1:61002\n";
    for unseen in [[conn, "", conn], [conn, listening, conn], ["", "", conn]] {
        let missed = seen_at(&unseen, &[4100]);
        assert!(missed.talked(5000), "{unseen:?}");
        assert!(
            !missed.egress_held(5000, 3, false),
            "a request whose connection was not seen: {unseen:?}"
        );
    }

    let out = seen(
        &format!("{conn}n192.168.1.9:61001->17.253.144.10:443\n"),
        &[4100],
        3,
    );
    assert_eq!(out.external().len(), 1, "{:?}", out.endpoints);
    assert!(!out.egress_held(5000, 3, false), "a socket off loopback");
    assert!(
        !good.egress_held(5000, 4, false),
        "a request not sampled at"
    );
    assert!(
        !seen(conn, &[4100], 0).egress_held(5000, 0, false),
        "no request at all"
    );
    assert!(
        !good.egress_held(5001, 3, false),
        "the model's port not seen"
    );
    assert!(
        !seen(conn, &[4200], 3).egress_held(5000, 3, false),
        "a pid outside the tree"
    );
    assert!(!good.egress_held(5000, 3, true), "the canary touched");
}

/// THE JAIL PROBE (review F1): under the lane's network rule ([`crate::codex_rig::jail_policy`])
/// a jailed `nc` reaches the model's port and is refused the canary's, which took nothing;
/// with the jail opened to every loopback port (parity-6's live negative control) it reaches
/// the canary, which saw it; with no network at all it does not reach the model either. And
/// the canary reads untouched until something connects to it.
#[test]
fn the_jail_probe_holds_a_jail_that_names_the_models_port_alone() {
    let listen = || {
        let listener = aterm_uds::kept_out(|| TcpListener::bind("127.0.0.1:0")).unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    };
    let (_model, port) = listen();
    let jail = crate::codex_rig::jail_policy(port);
    let (canary, _) = listen();
    assert!(jail_holds(&jail, port, &canary), "the lane's jail");
    assert!(!canary_touched(&canary), "the lane's jail kept the canary");

    let open = format!("{jail}(allow network-outbound (remote ip \"localhost:*\"))");
    let (canary, _) = listen();
    assert!(!jail_holds(&open, port, &canary), "a jail open to loopback");
    reached(&canary, "the open jail's probe reached it");
    let (canary, _) = listen();
    assert!(
        !jail_holds("(version 1)(allow default)(deny network*)", port, &canary),
        "no network"
    );

    let (canary, canary_port) = listen();
    assert!(!canary_touched(&canary));
    let _conn = aterm_uds::kept_out(|| TcpStream::connect(("127.0.0.1", canary_port)))
        .expect("a connection");
    reached(&canary, "a connection to it");
}

/// Reads `canary` touched once the connection that must reach it is in its queue. A loopback
/// `connect` (a probe's too) returns once its SYN-ACK is in, while the handshake's last ACK
/// can still wait behind other loopback traffic, so a nonblocking accept straight after it
/// can find nothing. The sanctioned accept waits for that queue under the one-minute hang
/// detector before taking the connection.
#[track_caller]
fn reached(canary: &TcpListener, what: &str) {
    let accepted = aterm_uds::accept(
        canary,
        aterm_uds::AcceptWait::Until(Instant::now() + MUST_HAPPEN),
        aterm_uds::AcceptedMode::Blocking,
    );
    assert!(
        accepted.is_ok(),
        "{what}: the canary's queue ({accepted:?})"
    );
}

/// THE CLI CHECKS (review F1): `exec --help` holds only on exit 0 with every flag the cases
/// pass named, `--version` only on exit 0 saying the package's exact version.
#[test]
fn the_cli_checks_hold_a_clean_exit_naming_the_flags_and_the_version() {
    let help = "Usage: codex exec [OPTIONS] [PROMPT]\n      --json\n      --color <COLOR>\n      \
                --skip-git-repo-check\n  -C, --cd <DIR>\n  -s, --sandbox <MODE>\n";
    assert!(help_names_the_flags(Some(0), help));
    for exit in [Some(1), Some(2), None] {
        assert!(!help_names_the_flags(exit, help), "{exit:?}");
    }
    for flag in EXEC_FLAGS {
        assert!(
            !help_names_the_flags(Some(0), &help.replace(flag, "--gone")),
            "{flag}"
        );
    }

    let said = format!("codex-cli {VERSION}\n");
    assert!(says_version(Some(0), &said, VERSION));
    for exit in [Some(1), None] {
        assert!(!says_version(exit, &said, VERSION), "{exit:?}");
    }
    for text in [
        "codex-cli 0.1.0-dev.2+codex.0.166.0.gabcdef012\n".to_string(),
        format!("codex-cli {VERSION}-extra\n"),
        format!("codex {VERSION}\n"),
        String::new(),
    ] {
        assert!(!says_version(Some(0), &text, VERSION), "{text:?}");
    }
}

/// EACH CASE'S CODEX (review F1) asks the lane's loopback model alone, with no login, and
/// retries nothing itself, so the retry the lane times is codex-alab's capacity recovery;
/// its updater, analytics, feedback and telemetry are off.
#[test]
fn each_cases_codex_asks_the_lanes_model_alone_and_leaves_retrying_to_codex_alab() {
    let doc: aterm_toml::Value = aterm_toml::from_str(&case_config(5000)).expect("valid TOML");
    assert_eq!(doc["model"].as_str(), Some(MODEL));
    assert_eq!(doc["model_provider"].as_str(), Some("capacity-smoke"));
    assert_eq!(doc["check_for_update_on_startup"].as_bool(), Some(false));
    let provider = &doc["model_providers"]["capacity-smoke"];
    assert_eq!(
        provider["base_url"].as_str(),
        Some("http://127.0.0.1:5000/v1")
    );
    assert_eq!(provider["requires_openai_auth"].as_bool(), Some(false));
    assert_eq!(provider["request_max_retries"].as_integer(), Some(0));
    assert_eq!(provider["stream_max_retries"].as_integer(), Some(0));
    for table in ["analytics", "feedback"] {
        assert_eq!(doc[table]["enabled"].as_bool(), Some(false), "{table}");
    }
    for exporter in ["exporter", "metrics_exporter", "trace_exporter"] {
        assert_eq!(doc["otel"][exporter].as_str(), Some("none"), "{exporter}");
    }
}

/// The verdict rows no test reached before (review F3): a second native turn, or none; a
/// recovered case whose turn did not complete, failed too or completed twice; a tool output
/// lost, under another call, or kept twice; and a plan tool whose output does not say it
/// updated the plan. CONTROL: the plan's output as a JSON value saying so passes, as the
/// unchanged case does in every test above.
#[test]
fn the_verdict_fails_a_turn_too_many_an_unrecovered_turn_a_lost_output_and_an_unrun_plan() {
    let good = events("agent_message", "item.completed");
    let ok = taken(13.0, 13.001, 15.001);
    let turn = |t: &str| J::obj([("type", J::s(t))]);
    let mut two = good.clone();
    two.insert(1, turn("turn.started"));
    let none: Vec<J> = good[1..].to_vec();
    for (said, events) in [("two", &two), ("none", &none)] {
        assert!(
            judge(Case::CapacityRecovery, Some(0), &ok, events)
                .unwrap_err()
                .contains("expected one native turn"),
            "{said}"
        );
    }
    let refusal = [
        turn("turn.started"),
        turn("turn.started"),
        turn("turn.failed"),
    ];
    assert!(
        judge(Case::PermanentRefusal, Some(1), &ok[..1], &refusal)
            .unwrap_err()
            .contains("expected one native turn")
    );

    let mut uncompleted = good.clone();
    uncompleted.pop();
    let mut failed = good.clone();
    failed.push(turn("turn.failed"));
    let mut twice = good.clone();
    twice.push(turn("turn.completed"));
    for (said, events) in [
        ("not completed", &uncompleted),
        ("failed too", &failed),
        ("completed twice", &twice),
    ] {
        assert!(
            judge(Case::CapacityRecovery, Some(0), &ok, events)
                .unwrap_err()
                .contains("turn did not recover"),
            "{said}"
        );
    }

    let with = |outputs: Vec<J>| {
        let mut t = taken(13.0, 13.001, 15.001);
        let mut input = vec![user()];
        input.extend(outputs);
        t[1].body = request(input.clone());
        t[2].body = request(input);
        t
    };
    let other_call = J::obj([
        ("type", J::s("function_call_output")),
        ("call_id", J::s("another-call")),
        ("output", J::s("Plan updated")),
    ]);
    for (said, outputs) in [
        ("lost", Vec::new()),
        ("another call's", vec![other_call]),
        ("kept twice", vec![output(), output()]),
    ] {
        assert!(
            judge(Case::CapacityRecovery, Some(0), &with(outputs), &good)
                .unwrap_err()
                .contains("completed tool output lost"),
            "{said}"
        );
    }

    let said = |out: J| {
        with(vec![J::obj([
            ("type", J::s("function_call_output")),
            ("call_id", J::s(CALL_ID)),
            ("output", out),
        ])])
    };
    for out in [
        J::s("Plan failed"),
        J::Null,
        J::obj([("ok", J::Bool(false))]),
    ] {
        assert!(
            judge(Case::CapacityRecovery, Some(0), &said(out.clone()), &good)
                .unwrap_err()
                .contains("plan tool did not complete"),
            "{out:?}"
        );
    }
    assert!(
        judge(
            Case::CapacityRecovery,
            Some(0),
            &said(J::obj([("content", J::s("Plan updated"))])),
            &good
        )
        .is_ok()
    );
}

const STOP_PROBE: &str = "ATERM_LAB_CAPACITY_STOP_PROBE";
const STOP_DIR: &str = "ATERM_LAB_CAPACITY_STOP_DIR";

/// An act of the lab's registered before the run's ([`stop::at_hard_exit`]).
fn act_registered_before() {
    let _ = crate::sys::write_all(1, b"PROBE the act registered before the run's ran\n");
}

/// AN INTERRUPT OF THE LAB ENDS THE RUN GOING AND WHAT IT LEFT IN ITS GROUP (review F4), each
/// probed in a copy of this test binary, since an interrupt is the whole process's: a first
/// ([`stop::note`], as the handler [`stop::install`] sets notes one) ends the wait at once
/// with [`Cut::Stopped`], its bound a minute off, the `sleep` the run left killed; a second
/// real `SIGINT`, which ends the lab at once (128 + 2), kills the held group first, and runs
/// the act the lab registered before the run's too. CONTROL: with the wait a plain sleep, the
/// first runs to its bound and reads as no exit; with no act at a second interrupt, the
/// `sleep` outlives the lab; with the run's act replacing the one before it, as
/// `stop::at_hard_exit` held one, that act never runs.
#[test]
fn an_interrupt_ends_the_run_and_what_it_left_in_its_group() {
    const TEST: &str = "codex_alab_proof::capacity_proof::tests::an_interrupt_ends_the_run_and_what_it_left_in_its_group";
    if let Some(mode) = std::env::var_os(STOP_PROBE) {
        let root = PathBuf::from(std::env::var_os(STOP_DIR).expect("the probe's scratch"));
        let mut child = waiting_run(&root);
        if mode == "first" {
            stop::note(libc::SIGINT);
            let ran = end_with_group(&mut child, &Witness::default(), MUST_HAPPEN);
            assert!(
                matches!(ran, Err(Cut::Stopped(Stopped(libc::SIGINT)))),
                "{ran:?}"
            );
            println!("PROBE the first interrupt ended the run");
            std::process::exit(0);
        }
        crate::sys::start_signal(libc::SIGINT, false);
        stop::install();
        stop::at_hard_exit(act_registered_before);
        hold(i32::try_from(child.id()).ok());
        let me = i32::try_from(std::process::id()).expect("pid");
        crate::sys::kill(me, libc::SIGINT).expect("the first interrupt");
        assert_eq!(
            stop::sleep(MUST_HAPPEN),
            Err(Stopped(libc::SIGINT)),
            "the first interrupt is noted"
        );
        crate::sys::kill(me, libc::SIGINT).expect("the second interrupt");
        std::thread::sleep(MUST_HAPPEN);
        println!("PROBE still running");
        std::process::exit(0);
    }
    for (mode, code, said) in [
        ("first", 0, "PROBE the first interrupt ended the run"),
        (
            "second",
            128 + libc::SIGINT,
            "PROBE the act registered before the run's ran",
        ),
    ] {
        let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
        let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
        cmd.args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(STOP_PROBE, mode)
            .env(STOP_DIR, tmp.path());
        let out = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + MUST_HAPPEN * 2)
            .expect("the probe ends within its hang detector");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(code), "{mode}: {stdout}");
        assert!(stdout.contains(said), "{mode}: {stdout}");
        ends(left(tmp.path()), &format!("at the {mode} interrupt"));
    }
}
