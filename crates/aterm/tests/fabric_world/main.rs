// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fabric worlds, one test binary: a guarded broker, a headless `aterm`
//! and the `aterm link serve` bridge it launches, driven end to end.
//!
//! The subject is THIS package's own binary (`harness::aterm`), so cargo
//! builds it, and rebuilds it after any edit below it, before a test here runs.
//! Every world scratches in an exclusive directory of its own
//! (`/tmp/atl-<random>`, `/tmp/a5-<random>`): private sockets, state and
//! HOME, so a world never meets another run's daemons or the machine's own.
//! `harness::boot_permit` caps the tests holding live stacks at once.
#![cfg(unix)]

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

mod harness;

mod bridge_e2e;
mod bridge_line_bound;
mod bridge_oversize;
mod bridge_recovery;
mod bridge_wake;
mod broadcast;
mod broker_lifeline;
mod broker_poison;
mod broker_retention;
mod doctor_retire;
mod exactly_once;
mod fabric_cli;
mod fabric_on;
mod handoff_cost;
mod link_state;
mod log_mail;
mod log_mirror;
mod ls_paging;
mod mail_gap;
mod notify;
mod one_state_dir;
mod ordered;
mod owner_ask;
mod presence;
mod receipt_edge_cases;
mod receipts;
mod replaced_log;
mod retention_live;
mod roster_bye;
mod sender_attestation;
mod subject_reclaim;
mod topic_gap;
mod topic_read_before_watch;
mod two_nodes;
