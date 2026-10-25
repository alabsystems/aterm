// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Physical-link evidence must agree before a LAN-only connection is admitted.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn lan_boundary_model() -> Model {
    aterm_spec::ty_model! {
        NetLanBoundary {
            const Buggy = 0;
            var scenario = 0;
            var admitted = 0;
            action Physical when (scenario == 0) { scenario = 1; admitted = if Buggy == 1 { 0 } else { 1 }; }
            action Routed when (scenario == 0) { scenario = 2; admitted = if Buggy == 2 { 1 } else { 0 }; }
            action Virtual when (scenario == 0) { scenario = 3; admitted = if Buggy == 3 { 1 } else { 0 }; }
            action WrongInterface when (scenario == 0) { scenario = 4; admitted = if Buggy == 4 { 1 } else { 0 }; }
            action WrongSource when (scenario == 0) { scenario = 5; admitted = if Buggy == 5 { 1 } else { 0 }; }
            action OffLink when (scenario == 0) { scenario = 6; admitted = if Buggy == 6 { 1 } else { 0 }; }
            action Down when (scenario == 0) { scenario = 7; admitted = if Buggy == 7 { 1 } else { 0 }; }
            action MissingScope when (scenario == 0) { scenario = 8; admitted = if Buggy == 8 { 1 } else { 0 }; }
            action MappedAddress when (scenario == 0) { scenario = 9; admitted = if Buggy == 9 { 1 } else { 0 }; }
            action ScopedIPv6 when (scenario == 0) { scenario = 10; admitted = if Buggy == 10 { 0 } else { 1 }; }
            invariant OnlyVerifiedLink: admitted == 0 || scenario == 1 || scenario == 10;
            invariant ValidLinkWorks: scenario == 0 || (scenario > 1 && scenario <= 9) || admitted == 1;
        }
    }
}

/// A TCP connect syscall is attempted only with a live deadline and intact
/// source-interface/hop policy. This does not attest to external packet paths.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn lan_prepared_dial_model() -> Model {
    aterm_spec::ty_model! {
        NetLanPreparedDial {
            const Buggy = 0;
            var scenario = 0;
            var attempted = 0;
            action IPv4 when (scenario == 0) { scenario = 1; attempted = if Buggy == 1 { 0 } else { 1 }; }
            action IPv6 when (scenario == 0) { scenario = 2; attempted = if Buggy == 2 { 0 } else { 1 }; }
            action Expired when (scenario == 0) { scenario = 3; attempted = if Buggy == 3 { 1 } else { 0 }; }
            action ChangedHops when (scenario == 0) { scenario = 4; attempted = if Buggy == 4 { 1 } else { 0 }; }
            action WrongBinding when (scenario == 0) { scenario = 5; attempted = if Buggy == 5 { 1 } else { 0 }; }
            invariant OnlyPreparedAttempts: attempted == 0 || scenario == 1 || scenario == 2;
            invariant PreparedCanAttempt: scenario == 0 || scenario > 2 || attempted == 1;
        }
    }
}

/// A listening socket cannot start answering SYNs until its interface evidence,
/// deadline, binding and hop limit have been checked.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn lan_prepared_listen_model() -> Model {
    aterm_spec::ty_model! {
        NetLanPreparedListen {
            const Buggy = 0;
            var scenario = 0;
            var attempted = 0;
            action IPv4 when (scenario == 0) { scenario = 1; attempted = if Buggy == 1 { 0 } else { 1 }; }
            action IPv6 when (scenario == 0) { scenario = 2; attempted = if Buggy == 2 { 0 } else { 1 }; }
            action Expired when (scenario == 0) { scenario = 3; attempted = if Buggy == 3 { 1 } else { 0 }; }
            action ChangedHops when (scenario == 0) { scenario = 4; attempted = if Buggy == 4 { 1 } else { 0 }; }
            action WrongBinding when (scenario == 0) { scenario = 5; attempted = if Buggy == 5 { 1 } else { 0 }; }
            action StaleAttachment when (scenario == 0) { scenario = 6; attempted = if Buggy == 6 { 1 } else { 0 }; }
            action LoopbackIPv4 when (scenario == 0) { scenario = 7; attempted = if Buggy == 7 { 0 } else { 1 }; }
            action LoopbackIPv6 when (scenario == 0) { scenario = 8; attempted = if Buggy == 8 { 0 } else { 1 }; }
            action IPv6ChangedHops when (scenario == 0) { scenario = 9; attempted = if Buggy == 9 { 1 } else { 0 }; }
            action IPv6WrongBinding when (scenario == 0) { scenario = 10; attempted = if Buggy == 10 { 1 } else { 0 }; }
            action WrongSource when (scenario == 0) { scenario = 11; attempted = if Buggy == 11 { 1 } else { 0 }; }
            action DualStack when (scenario == 0) { scenario = 12; attempted = if Buggy == 12 { 1 } else { 0 }; }
            action IPv6Expired when (scenario == 0) { scenario = 13; attempted = if Buggy == 13 { 1 } else { 0 }; }
            invariant OnlyPreparedAttempts: attempted == 0 || scenario == 1 || scenario == 2 || scenario == 7 || scenario == 8;
            invariant PreparedCanAttempt: scenario == 0 || (scenario > 2 && scenario <= 6) || scenario > 8 || attempted == 1;
        }
    }
}
