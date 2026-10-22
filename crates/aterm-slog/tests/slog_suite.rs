// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-slog integration files as modules of ONE test binary.
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.
//!
//! Run one file's tests with
//! `targo --unverified test -p aterm-slog --test slog_suite -- <file>::`.

mod support;

mod brief_budget;
mod carry;
mod claims;
mod close_ledger;
mod conformance_claims;
mod conformance_cursor;
mod conformance_fold;
mod conformance_journal;
mod conformance_mail_link;
mod conformance_node_log;
mod conformance_program_share;
mod conformance_quota;
mod conformance_ring;
mod conformance_rows;
mod conformance_transcript;
mod conformance_turn;
mod discipline;
mod fold_laws;
mod journal_codec;
mod journal_view;
mod kept_copy;
mod line_codec;
mod mark_quota;
mod mirror;
mod node_log;
mod program_share;
mod record_mono;
mod ring_bounds;
mod row_floor;
mod seq_replay;
mod seq_twins;
mod transcript_replays;
mod turn_cause;
mod turn_replays;
mod vocab_mirror;
