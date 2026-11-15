// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ALAB POLICY: the turn-end readers a harness judges an agent's reply with, over its words
//! alone. What a reply asks of a person (`NEEDS YOU:` lines, credentials, a go-ahead on its own
//! reversible work), whether it defends a landing (`Nothing left:`) and what it still names open,
//! Land's check and its answer, the carry that keeps a Cruise run's background work going, the
//! press guard, whether a shell command only reads, the grammar of the tab IDs a reply names
//! sessions by, and the words each act types. Every function is pure and names no agent program:
//! the screen readers that turn a program's screen into these words, and the engine that types the
//! acts, are the harness's own. What a screen reader reports to the policy (the agent's phase,
//! the wall its turn ended on, the kinds of limit reset and a reset's clock) is plain data here
//! ([`vocab`]), and so are the policy keys and timings a harness decides under ([`config`]).
//!
//! It is std only: aterm's engine and any other harness that carries it read the same
//! decisions.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// THE HARNESS'S MARK, as a literal for `concat!`: every turn a harness types into an agent's
/// conversation begins with it.
#[macro_export]
macro_rules! mark {
    () => {
        "[aterm harness]"
    };
}

pub mod classify;
pub mod config;
pub mod credential_need;
pub mod cruise_carry;
pub mod go_ahead;
pub mod guard;
pub mod ladder;
pub mod land_check;
pub mod needs;
pub mod open_work;
pub mod tab_id;
pub mod text;
pub mod vocab;
pub mod words;

/// The corpora the readers are judged on, for tests here and in dependent crates: the parsers of
/// the go-ahead's table, the open-work corpora and the needs table, each over the text its test
/// includes from `fixtures/`, and the generator and fields of the guard's base record. Not part
/// of the documented API.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub mod corpus;
