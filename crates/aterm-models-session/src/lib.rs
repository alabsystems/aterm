// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// Registers the `trust` tool namespace under verify builds only, so `#[trust::…]` attributes
// compile everywhere and mean something under `targo trust build`.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm's models of sessions, the fabric and the message bus.
//!
//! Each `*_model()` is a bounded state machine written once with `aterm_spec::ty_model!` (or a
//! hand-built `aterm_spec::derive::Model` under `#[aterm_spec::model]`); writing it registers it.
//! The engine (the macro, the interpreter, `verify`, `xref`) is `aterm-spec`; `aterm-models`
//! links every domain crate for `spec-check` and the registry-wide sweeps. Only the crates whose
//! tests drive these models dev-depend on this one, so editing a model here rebuilds this crate
//! and those tests, never a library.

mod models_broadcast_checkpoint;
mod models_broadcast_head;
mod models_crash_restore;
mod models_exceptions_digest;
mod models_exceptions_ends;
mod models_exceptions_once;
mod models_fabric_enable;
mod models_fabric_outbox_wake;
mod models_fabric_resume;
mod models_fork_exec;
mod models_incarnation_lease;
mod models_input;
mod models_log_mirror;
mod models_notify_follow;
mod models_paste_order;
mod models_say_replay_budget;
mod models_session;
mod models_session_log;
mod models_subscribe_announcement;
mod models_write_all;

pub use models_broadcast_checkpoint::broadcast_cursor_checkpoint_model;
pub use models_broadcast_head::broadcast_head_subscription_model;
pub use models_crash_restore::crash_restore_journal_model;
pub use models_exceptions_digest::exceptions_digest_model;
pub use models_exceptions_ends::{exceptions_digest_ends_liveness, exceptions_digest_ends_model};
pub use models_exceptions_once::exceptions_once_model;
pub use models_fabric_enable::*;
pub use models_fabric_outbox_wake::{
    fabric_outbox_wake_model, fabric_reconnect_backoff_model, fabric_refill_retry_model,
};
pub use models_fabric_resume::fabric_reader_resume_model;
pub use models_fork_exec::fork_exec_model;
pub use models_incarnation_lease::incarnation_lease_model;
pub use models_input::input_unread_gate_model;
pub use models_log_mirror::log_mirror_model;
pub use models_notify_follow::notify_follow_checkpoint_model;
pub use models_paste_order::*;
pub use models_say_replay_budget::say_replay_budget_model;
pub use models_session::*;
pub use models_session_log::{
    session_log_claims_model, session_log_cursor_model, session_log_journal_model,
    session_log_mark_quota_model, session_log_ring_model, turn_tracker_model,
};
pub use models_subscribe_announcement::subscribe_announcement_order_model;
pub use models_write_all::write_all_model;
