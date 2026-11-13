// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn user(text: &str) -> J {
    J::obj([("role", J::s("user")), ("content", J::s(text))])
}

fn output() -> J {
    J::obj([
        ("type", J::s("function_call_output")),
        ("call_id", J::s("call_1")),
        ("output", J::s("Process running with session ID 1")),
    ])
}

fn request(items: Vec<J>) -> J {
    J::obj([("input", J::Arr(items))])
}

fn opening() -> J {
    user(&format!("LEG-{LEG} Start the two watchers."))
}

fn tag(body: &J) -> String {
    answer(body, LEG).expect("the terminals leg answers").tag
}

fn command(answered: &Answered) -> String {
    let args = answered
        .body
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|l| J::parse(l).ok())
        .find_map(|event| {
            event
                .get("item")
                .filter(|it| it.get("type").and_then(J::as_str) == Some("function_call"))
                .and_then(|it| it.get("arguments"))
                .and_then(J::as_str)
                .map(str::to_owned)
        })
        .expect("a function call");
    J::parse(&args)
        .expect("its arguments")
        .get("cmd")
        .and_then(J::as_str)
        .expect("its cmd")
        .to_owned()
}

/// Each step of the leg answers as the proof expects: the two watchers in turn (the first
/// held until the proof's hold stands), the turn's end once both run, the foreground command
/// a typed quick check starts and its end once its output is in, and anything else marked.
#[test]
fn the_terminals_leg_starts_two_watchers_then_a_foreground_command() {
    let first = answer(&request(vec![opening()]), LEG).expect("the first answer");
    assert_eq!(first.tag, "terminals:first-terminal");
    assert!(first.gate.is_some(), "the first answer waits for the hold");
    assert!(command(&first).contains(STOP_A), "{}", first.body);
    let second = answer(&request(vec![opening(), output()]), LEG).expect("the second");
    assert_eq!(second.tag, "terminals:second-terminal");
    assert!(second.gate.is_none());
    assert!(command(&second).contains(STOP_B), "{}", second.body);
    assert_eq!(
        tag(&request(vec![opening(), output(), output()])),
        "terminals:after-terminals"
    );
    let quick = vec![opening(), output(), output(), user(QUICK_CHECK)];
    let foreground = answer(&request(quick.clone()), LEG).expect("the foreground call");
    assert_eq!(foreground.tag, "terminals:foreground");
    assert!(command(&foreground).contains(STOP_C), "{}", foreground.body);
    let mut done = quick;
    done.push(output());
    assert_eq!(tag(&request(done)), "terminals:after-foreground");
    assert_eq!(
        tag(&request(vec![opening(), user("keep going")])),
        "terminals:unexpected"
    );
}

/// Only the terminals leg is answered here, and never its side requests.
#[test]
fn every_other_leg_and_a_side_request_are_left_to_the_main_model() {
    for leg in ["first", "policy", "usage", "-"] {
        assert!(answer(&request(vec![opening()]), leg).is_none(), "{leg}");
    }
    let side = J::obj([
        ("input", J::Arr(vec![opening()])),
        (
            "text",
            J::obj([("format", J::obj([("type", J::s("json_schema"))]))]),
        ),
    ]);
    assert!(answer(&side, LEG).is_none());
}
