// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Responses for the background terminals proof (`codex-alab-proof --terminals-only`, gap 28):
//! two `exec_command` watchers that outlive their yield, then a command a running call waits on
//! in the foreground. Every watcher ends only when the proof creates its stop file in the leg's
//! folder, so no check races a timer.

use std::sync::{Arc, OnceLock};

use crate::codex_rig::{Answered, ResponseGate, call_item, message_item, request_view};
use crate::pyjson::J;

/// The leg this model answers.
pub const LEG: &str = "terminals";

/// The file whose creation ends the first watcher.
pub const STOP_A: &str = "stop-a";

/// The file whose creation ends the second watcher.
pub const STOP_B: &str = "stop-b";

/// The file whose creation ends the foreground command.
pub const STOP_C: &str = "stop-c";

/// What the proof types to start the foreground command's turn.
pub const QUICK_CHECK: &str = "Run the quick check.";

/// The reply that ends the first turn with both watchers running.
pub const TERMINALS_RUN: &str = "Two watchers run in the background.";

/// The reply once the foreground command's output is in.
pub const QUICK_DONE: &str = "The quick check passed.";

/// The shortest yield codex clamps an `exec_command` to: a watcher outlives it at once.
const BACKGROUND_YIELD_MS: i64 = 250;

/// The longest yield codex clamps an `exec_command` to: the call waits on its command for it.
const FOREGROUND_YIELD_MS: i64 = 30_000;

/// Holds the leg's first answer until the proof's hold stands.
static FIRST: OnceLock<Arc<ResponseGate>> = OnceLock::new();

/// Lets the leg's first answer go: the proof's hold stands.
pub fn release_first() {
    FIRST.get_or_init(Default::default).release();
}

/// An `exec_command` that runs in the leg's folder until `file` is there, its call yielding after
/// `yield_ms`.
fn waits_for(file: &str, yield_ms: i64) -> J {
    call_item(
        "exec_command",
        &J::obj([
            (
                "cmd",
                J::s(format!("while [ ! -e {file} ]; do sleep 0.2; done")),
            ),
            ("yield_time_ms", J::Int(yield_ms)),
        ]),
    )
}

/// How many tool outputs the request carries.
fn outputs(body: &J) -> usize {
    body.get("input")
        .and_then(J::as_arr)
        .unwrap_or_default()
        .iter()
        .filter(|it| it.get("type").and_then(J::as_str) == Some("function_call_output"))
        .count()
}

/// The terminals leg's answer to `body`, or `None` for every other leg and for a side request.
#[must_use]
pub fn answer(body: &J, leg: &str) -> Option<Answered> {
    if leg != LEG {
        return None;
    }
    let format = body.get("text").and_then(|t| t.get("format"));
    if format.and_then(|f| f.get("type")).and_then(J::as_str) == Some("json_schema") {
        return None;
    }
    let say = |text: &str| Answered::items(vec![message_item(text)]);
    let call = |item: J| Answered::items(vec![item]);
    let (last, tool) = request_view(body);
    let opening = last.starts_with(&format!("LEG-{LEG} "));
    let (mut answer, step) = if last.contains(QUICK_CHECK) && tool.is_some() {
        (say(QUICK_DONE), "after-foreground")
    } else if last.contains(QUICK_CHECK) {
        (call(waits_for(STOP_C, FOREGROUND_YIELD_MS)), "foreground")
    } else if opening && outputs(body) == 0 {
        let mut first = call(waits_for(STOP_A, BACKGROUND_YIELD_MS));
        first.gate = Some(Arc::clone(FIRST.get_or_init(Default::default)));
        (first, "first-terminal")
    } else if opening && outputs(body) == 1 {
        (
            call(waits_for(STOP_B, BACKGROUND_YIELD_MS)),
            "second-terminal",
        )
    } else if opening {
        (say(TERMINALS_RUN), "after-terminals")
    } else {
        (say("Nothing more to do."), "unexpected")
    };
    answer.tag = format!("{LEG}:{step}");
    Some(answer)
}

#[cfg(test)]
#[path = "codex_alab_terminals_model_tests.rs"]
mod tests;
