// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A local signed-entry read, made in place from the verified store, and re-observation of an
/// opaque snapshot. A file at an entry's old `~/.secrets` path, edited, gone or behind a link,
/// changes no read (`FileIgnored`, DESIGN-secrets §7): the read returns the vault's bytes. These
/// bounded scenarios grant no provider access and assert no global freshness. The typed read of a
/// Claude Code token entry (`ClaudeToken`, DESIGN-claude-setup-token §4.2) accepts the class's
/// value in place, while an ordinary read of a class name (`ClaudeTokenOrdinary`) is refused like
/// any other reserved name.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn verified_entry_model() -> Model {
    aterm_spec::ty_model! {
        DbVerifiedEntry {
            const Buggy = 0;
            var scenario = 0; var accepted = 0; var effects = 0;
            action Valid when (scenario == 0) {scenario = 1; accepted = if Buggy == 1 {0} else {1}; effects = if Buggy == 19 {1} else {0};}
            action FileIgnored when (scenario == 0) {scenario = 2; accepted = if Buggy == 2 {0} else {1}; effects = if Buggy == 3 {1} else {0};}
            action Cache when (scenario == 0) {scenario = 4; accepted = if Buggy == 4 {1} else {0};}
            action Config when (scenario == 0) {scenario = 5; accepted = if Buggy == 5 {1} else {0};}
            action Busy when (scenario == 0) {scenario = 6; accepted = if Buggy == 6 {1} else {0};}
            action Revoked when (scenario == 0) {scenario = 7; accepted = if Buggy == 7 {1} else {0};}
            action Partial when (scenario == 0) {scenario = 8; accepted = if Buggy == 8 {1} else {0};}
            action Unsigned when (scenario == 0) {scenario = 9; accepted = if Buggy == 9 {1} else {0};}
            action Unrelated when (scenario == 0) {scenario = 10; accepted = if Buggy == 10 {0} else {1};}
            action Changed when (scenario == 0) {scenario = 11; accepted = if Buggy == 11 {1} else {0};}
            action Policy when (scenario == 0) {scenario = 12; accepted = if Buggy == 12 {1} else {0};}
            action Rollback when (scenario == 0) {scenario = 13; accepted = if Buggy == 13 {1} else {0};}
            action Recipient when (scenario == 0) {scenario = 14; accepted = if Buggy == 14 {1} else {0};}
            action Fork when (scenario == 0) {scenario = 16; accepted = if Buggy == 16 {1} else {0};}
            action Reserved when (scenario == 0) {scenario = 18; accepted = if Buggy == 18 {1} else {0};}
            action ClaudeToken when (scenario == 0) {scenario = 19; accepted = if Buggy == 20 {0} else {1};}
            action ClaudeTokenOrdinary when (scenario == 0) {scenario = 20; accepted = if Buggy == 21 {1} else {0};}
            invariant OnlyBoundBytes: scenario == 0 || scenario == 1 || scenario == 2 || scenario == 10 || scenario == 19 || accepted == 0;
            invariant Available: ((1 > scenario || scenario > 2) && (10 > scenario || scenario > 10) && (19 > scenario || scenario > 19)) || accepted == 1;
            invariant ReadOnly: effects == 0;
        }
    }
}
