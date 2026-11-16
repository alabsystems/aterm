// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ALAB POLICY: the turn-end readers a harness judges an agent's reply with, over its words
//! alone. What a reply asks of a person (`NEEDS YOU:` lines, credentials, a go-ahead on its own
//! reversible work), whether it defends a landing (`Nothing left:`) and what it still names open,
//! Land's check and its answer, the carry that keeps a Cruise run's background work going, the
//! press guard, whether a shell command only reads, the grammar of the tab IDs a reply names
//! sessions by, and the words each act types. Every function is pure, and the readers name no
//! agent program: the screen readers that turn a program's screen into these words, and the
//! engine that types the acts, are the harness's own. What a screen reader reports to the policy
//! (the agent's phase, the wall its turn ended on, the kinds of limit reset and a reset's clock)
//! is plain data here ([`vocab`]), and so are the policy keys and timings a harness decides under
//! ([`config`]).
//!
//! Beside the readers sit the machine's sleeps inside a turn's done row ([`sleeps`]), the
//! git-config hazard check a read-only git command is approved under ([`git_config`]) and Claude
//! Code's shell startup ([`claude_code::shell_startup`]). Where a decider needs a fact that differs
//! from one agent to the next, it asks it through the [`port::Agent`] seam, and [`claude_code`]
//! holds Claude Code's facts.
//!
//! It is std only: aterm's engine and any other harness that carries it read the same
//! decisions. It has no dependencies, no `unsafe` code and no input or output. Its `clippy.toml`
//! refuses the std doors to files, processes, the environment, the network, the clock and sleep;
//! a caller passes what it read and the time it means.
//!
//! # Where each module came from
//!
//! The modules of the first row are the one copy of what aterm's own `crates/alab-policy` held,
//! and they are edited here. aterm's HX-21 swap deletes that crate and links a tag of this
//! repository instead, vendored and pinned by its tree id (aterm's `vendor/alab-harness.pin`
//! names the tag it carries), and claude-code-alab takes aterm's `verify vendor-out` relay of
//! that copy; each takes a change by a new tag. [`sleeps`], [`git_config`] and [`claude_code`]
//! are not the one copy yet: aterm-agent still keeps its own whole `sleeps.rs`, `git_config.rs`
//! and `shell_startup.rs`, pure halves included, and calls none of these three modules until
//! HX-21c's aterm half (its steps B5 and B6) moves it onto them. Until then a fix to one of their
//! rules is made in aterm-agent as well, and only aterm-agent's copy decides what aterm does.
//!
//! Each row names the aterm revision its files were copied from, which is the `Moved-From:`
//! trailer of the commit that copied them (aterm's paths are relative to its root). A copy
//! commit holds aterm's bytes unchanged, and the commit after it makes the fit the row's last
//! column names; for the three HX-21c modules that column says which half aterm keeps once B5
//! and B6 land.
//!
//! | Module | From aterm | Moved as |
//! |---|---|---|
//! | [`ladder`], [`classify`], [`credential_need`], [`cruise_carry`], [`go_ahead`], [`guard`], [`land_check`], [`needs`], [`open_work`], [`tab_id`], [`text`], [`vocab`], [`words`], [`config`], `corpus` and the crate's `fixtures/` | `crates/alab-policy/src` and `crates/alab-policy/fixtures` at 6863e1006d47 (that crate's tree d8fdbe4985ee) | whole files with their tests. The fit adds only the `#[expect(clippy::disallowed_methods)]`s this crate's `clippy.toml` asks of the tests that read a file, the environment or the clock. aterm had cut that crate out of aterm-agent's `supervise/` and `supervise/policy/` (8e707ad5fdd); its files replaced this crate's earlier copies of the same modules, made from aterm-agent at 700929979056, with their seams |
//! | [`port`] | none | new: the [`port::Agent`] trait a decider asks an agent's own facts through |
//! | [`sleeps`] | `crates/aterm-agent/src/supervise/policy/sleeps.rs`, `sleeps_tests.rs` at 5b2f8a35f2a7 | whole file but `done_row_step`, now [`claude_code::done_row_step`], which [`sleeps::SleepBook::in_turn`] asks of a [`port::Agent`]; and `Boot::{kernel, of, text}`, which spell a boot id by `astream_sys` and are to stay in aterm as its `BootSpelling`, with the two tests that spell one |
//! | [`claude_code`] | `crates/aterm-agent/src/supervise/policy/sleeps.rs` (`done_row_step`), `git_config.rs` (`claude_dir_at`), `git_config_tests.rs` (its test), `shell_startup.rs` (`CLAUDE_PART`, and the rest of its pure half as [`claude_code::shell_startup`]) and `shell_startup_tests.rs` at 5b2f8a35f2a7 | item cuts; aterm is to keep `WorkerEnv::claude_dir`, which reads the worker's environment and account home, calling [`claude_code::claude_dir_at`]; and `shell_startup`'s live half, in its file of the same name: `read`'s checks and its gathering of a [`claude_code::shell_startup::StartupInputs`], to hand to [`claude_code::shell_startup::ShellStartup::assemble`] (`snapshots`, `hook_env_scripts`, `zshenv`, `bash_env`, `tool_kinds`, the `MAX_*` bounds), with the tests that read files or the worker's environment |
//! | [`git_config`] | `crates/aterm-agent/src/supervise/policy/git_config.rs`, `git_config_tests.rs`, and `approval.rs` (`rooted`) at 5b2f8a35f2a7 | item cuts: the pure half. aterm is to keep the live half, which starts git to read a [`git_config::GitView`] (`WorkerEnv` with `digest`, `Probe`, `git_output*`, `is_executable`, `claude_dir`), in its file of the same name, with the tests that run real git or read a `WorkerEnv` |
//! | `rng` (tests only) | `crates/aterm-spec/src/rng.rs` at 5b2f8a35f2a7 | whole file: the deterministic generator the moved tests of generated inputs draw from, so they draw the same inputs as in aterm |

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
pub mod claude_code;
pub mod config;
pub mod credential_need;
pub mod cruise_carry;
pub mod git_config;
pub mod go_ahead;
pub mod guard;
pub mod ladder;
pub mod land_check;
pub mod needs;
pub mod open_work;
pub mod port;
#[cfg(test)]
mod rng;
pub mod sleeps;
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
