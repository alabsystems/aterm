// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fabric worlds, one test binary: a guarded broker, a headless `aterm`
//! and the `aterm link serve` bridge it launches, driven end to end.
//!
//! The subject is THIS package's own binary (`CARGO_BIN_EXE_aterm`), so cargo
//! builds it, and rebuilds it after any edit below it, before a test here runs.
//! Every world scratches under its own tag and this process's pid
//! (`/tmp/atl-<tag>-<pid>`, `/tmp/a5-<tag><pid>`): private sockets, state and
//! HOME, so a world never meets another run's daemons or the machine's own.
//! `harness::boot_permit` caps the tests holding live stacks at once.
//!
//! `two_nodes_sealed` is a target of its own: it needs `--features sealed`.
#![cfg(unix)]

mod harness;

mod bridge_e2e;
mod bridge_line_bound;
mod bridge_oversize;
mod bridge_recovery;
mod bridge_wake;
mod broadcast;
mod doctor_retire;
mod exactly_once;
mod fabric_cli;
mod fabric_on;
mod link_state;
mod log_mirror;
mod ls_paging;
mod notify;
mod one_state_dir;
mod presence;
mod receipt_edge_cases;
mod receipts;
mod replaced_log;
mod second_host;
mod sender_attestation;
mod topic_gap;
mod topic_read_before_watch;
