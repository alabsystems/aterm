// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Integration test harness for `agh`.

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

mod common;

mod args;
#[cfg(unix)]
mod auth_verbs;
mod classification;
mod cli_reads;
mod client_pipeline;
mod config_rules;
mod download;
mod errors;
mod fx_seam;
mod golden_cli;
mod help_schema;
mod hop;
#[cfg(unix)]
mod identity_files;
mod jq_corpus;
mod output;
mod pages;
mod policy_table;
mod rate_book;
mod redaction;
mod res_contents;
mod res_git;
mod res_public;
mod res_releases;
mod res_repos;
mod retry_table;
mod slugs;
mod tape_fixtures;
mod tapes;
mod users_rate;
#[cfg(unix)]
mod vault_adoption;
mod waits;
mod wire_loopback;
mod wire_types;
