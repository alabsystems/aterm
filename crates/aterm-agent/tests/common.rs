// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the supervise conformance binds share (`agent_suite`'s modules): the
//! walks stay per model.

use aterm_agent::supervise::policy::turn_end::TurnEndState;
use aterm_spec::derive::Model;

/// The model's constant `name`; a name it does not declare panics.
pub fn cnst(m: &Model, name: &str) -> i64 {
    m.consts
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| *v)
        .unwrap_or_else(|| panic!("no const {name}"))
}

/// A turn's words before its last, the `n`th a walk draws: each its own reply to the real
/// `TurnEndState::repeats`, in letters (a word with a digit reads as one number).
pub fn earlier(n: usize) -> String {
    let mut word = String::new();
    let mut k = n + 1;
    while k > 0 {
        k -= 1;
        word.insert(0, char::from(b'a' + u8::try_from(k % 26).unwrap_or(0)));
        k /= 26;
    }
    format!("Read part {word}.")
}

/// The model's `same` the real replies stand for: 0 none, 1 a no-work reply kept, 2 held. A wall's
/// own reply is 0: the models count no wall's tries (`Kept::walled`).
pub fn same(st: &TurnEndState) -> i64 {
    st.replies()
        .last()
        .filter(|k| !k.walled)
        .map_or(0, |k| if k.holds() { 2 } else { 1 })
}
