// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Integration test harness for `agh`.

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

mod common;

// unix-only: the write journal, git, the fabric socket or token files (§1, rule 12).
#[cfg(unix)]
mod api_writes;
mod args;
#[cfg(unix)]
mod atomic_refs;
#[cfg(unix)]
mod auth_verbs;
mod classification;
mod cli_reads;
#[cfg(unix)]
mod cli_writes;
mod client_pipeline;
mod config_rules;
mod download;
#[cfg(unix)]
mod ensure_once;
mod errors;
mod fx_seam;
#[cfg(unix)]
mod git_helper;
#[cfg(unix)]
mod git_runner;
mod golden_cli;
mod help_schema;
mod hop;
#[cfg(unix)]
mod identity_files;
#[cfg(unix)]
mod import_gh;
mod inline_source;
#[cfg(unix)]
mod intents_cli;
#[cfg(unix)]
mod journal;
mod jq_corpus;
#[cfg(unix)]
mod keys;
#[cfg(unix)]
mod notify;
mod output;
mod pages;
mod permits;
mod policy_table;
#[cfg(unix)]
mod policy_writers;
mod rate_book;
mod redaction;
#[cfg(unix)]
mod refs;
#[cfg(unix)]
mod release_writes;
#[cfg(unix)]
mod repo_create;
mod res_contents;
mod res_git;
mod res_public;
mod res_releases;
mod res_repos;
mod retry_table;
mod slugs;
mod tape_fixtures;
mod tapes;
#[cfg(unix)]
mod tapes_m2;
mod users_rate;
#[cfg(unix)]
mod vault_adoption;
mod waits;
mod wire_loopback;
mod wire_types;
#[cfg(unix)]
mod write_pipeline;
