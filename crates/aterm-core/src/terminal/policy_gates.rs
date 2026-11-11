// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Compiled per-gate policy decisions resolved once per installed [`PolicyEngine`].

use aterm_policy::engine::PolicyEngine;
use aterm_policy::limits::{RateLimitSlot, TimeSource};
use aterm_policy::{OriginTag, RateLimit};

use super::policy_bridge::{self, BridgeDecision};
use super::response_capability::ProbeKind;

/// The origin every compiled gate is resolved at.
pub(super) const GATE_ORIGIN: OriginTag = OriginTag::Pty;

const XTWINOPS_MAX_PS: u16 = 24;

/// The rate-limit id the response sink debits once per reply ([`PolicyState`], [`RateLimitSlot`]).
pub(super) const RESPONSE_LIMIT_ID: &str = "response";

/// The rate-limit id a program notification (OSC 9, 99 or 777) debits once.
pub(super) const NOTIFICATIONS_LIMIT_ID: &str = "notifications";

const XTWINOPS_GATES: usize = XTWINOPS_MAX_PS as usize + 1;
const SHELL_MAJORS: [u32; 2] = [133, 633];
const SHELL_SUBCOMMANDS: usize = 26;

/// The shell-integration gate verdict for one `(major, subcommand)`, live.
#[inline]
#[must_use]
fn shell_verdict(engine: Option<&PolicyEngine>, command: u32, subcommand: &str) -> BridgeDecision {
    policy_bridge::engine_decision(
        engine,
        &super::shell_integration_auth::probe_shell_integration(command, subcommand),
        GATE_ORIGIN,
    )
}

/// Table coordinates for a `(major, subcommand)` pair, or `None` when the pair is outside the
/// compiled range and must be evaluated live.
#[inline]
fn shell_slot(command: u32, subcommand: &str) -> Option<(usize, usize)> {
    let major = SHELL_MAJORS.iter().position(|&m| m == command)?;
    let &[byte] = subcommand.as_bytes() else {
        return None;
    };
    if !byte.is_ascii_uppercase() {
        return None;
    }
    Some((major, usize::from(byte - b'A')))
}

/// The XTWINOPS gate verdict for one `Ps`, evaluated live ([`PolicyState`]).
#[inline]
#[must_use]
pub(super) fn xtwinops_verdict(engine: Option<&PolicyEngine>, ps: u16) -> BridgeDecision {
    policy_bridge::engine_decision_deny_by_default_capability(
        engine,
        &super::window_auth::probe_xtwinops(ps),
        GATE_ORIGIN,
    )
}

/// Policy verdicts for the fixed capability gates, resolved once per policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PolicyGates {
    /// Verdict for the single `send_response` sink (`handler.rs`).
    response_sink: BridgeDecision,
    /// Verdict for the OSC 52 clipboard *set* gate (which also gates *clear*).
    osc52_set: BridgeDecision,
    /// Verdict for the OSC 52 clipboard *query* gate.
    osc52_query: BridgeDecision,
}

impl PolicyGates {
    /// Resolve every gate against `engine` ([`BridgeDecision::Fallback`]).
    pub(super) fn compile(engine: Option<&PolicyEngine>) -> Self {
        Self {
            response_sink: policy_bridge::engine_decision(
                engine,
                &super::response_capability::probe_for(ProbeKind::response_sink()),
                GATE_ORIGIN,
            ),
            osc52_set: policy_bridge::engine_decision_deny_by_default_capability(
                engine,
                &super::clipboard_auth::probe_osc52_set(),
                GATE_ORIGIN,
            ),
            osc52_query: policy_bridge::engine_decision_deny_by_default_capability(
                engine,
                &super::clipboard_auth::probe_osc52_query(),
                GATE_ORIGIN,
            ),
        }
    }

    /// Verdict for the response sink ([`BridgeDecision::resolve`]).
    #[inline]
    pub(super) const fn response_sink(self) -> BridgeDecision {
        self.response_sink
    }

    /// Verdict for the OSC 52 clipboard *set* (and *clear*) gate.
    #[inline]
    pub(super) const fn osc52_set(self) -> BridgeDecision {
        self.osc52_set
    }

    /// Verdict for the OSC 52 clipboard *query* gate.
    #[inline]
    pub(super) const fn osc52_query(self) -> BridgeDecision {
        self.osc52_query
    }
}

/// The terminal's policy.
#[derive(Debug, Clone)]
pub(super) struct PolicyState {
    /// The installed engine, or `None` when the host has not installed a policy.
    engine: Option<PolicyEngine>,
    /// Gate verdicts compiled from `engine` ([`Self::install`]).
    gates: PolicyGates,
    /// XTWINOPS verdicts for `Ps` 0..=[`XTWINOPS_MAX_PS`] ([`PolicyGates`]).
    xtwinops: [BridgeDecision; XTWINOPS_GATES],
    /// OSC 133 / OSC 633 verdicts, indexed by [`SHELL_MAJORS`] then by subcommand letter.
    shell: [[BridgeDecision; SHELL_SUBCOMMANDS]; SHELL_MAJORS.len()],
    /// The [`RESPONSE_LIMIT_ID`] bucket's slot in `engine`'s limiter set ([`Default`]).
    response_limit: RateLimitSlot,
    /// The [`NOTIFICATIONS_LIMIT_ID`] bucket's slot in `engine`'s limiter set.
    notifications_limit: RateLimitSlot,
}

impl PolicyState {
    /// No policy installed.
    #[inline]
    pub(super) fn new() -> Self {
        Self {
            engine: None,
            gates: PolicyGates::compile(None),
            xtwinops: compile_xtwinops(None),
            shell: compile_shell(None),
            response_limit: RateLimitSlot::UNDECLARED,
            notifications_limit: RateLimitSlot::UNDECLARED,
        }
    }

    /// Install (or replace) the engine, recompiling every gate table.
    pub(super) fn install(&mut self, engine: PolicyEngine) {
        self.gates = PolicyGates::compile(Some(&engine));
        self.xtwinops = compile_xtwinops(Some(&engine));
        self.shell = compile_shell(Some(&engine));
        self.response_limit = engine.rate_limit_slot(RESPONSE_LIMIT_ID);
        self.notifications_limit = engine.rate_limit_slot(NOTIFICATIONS_LIMIT_ID);
        self.engine = Some(engine);
    }

    /// OSC 133 / OSC 633 gate verdict for this dispatch.
    #[inline]
    pub(super) fn shell_integration_gate(&self, command: u32, params: &[&[u8]]) -> BridgeDecision {
        let subcommand = super::shell_integration_auth::probe_subcommand(params);
        shell_slot(command, subcommand).map_or_else(
            || shell_verdict(self.engine.as_ref(), command, subcommand),
            |(major, sub)| self.shell[major][sub],
        )
    }

    /// XTWINOPS verdict for `ps` ([`XTWINOPS_MAX_PS`]).
    #[inline]
    pub(super) fn xtwinops_gate(&self, ps: u16) -> BridgeDecision {
        self.xtwinops
            .get(usize::from(ps))
            .copied()
            .unwrap_or_else(|| xtwinops_verdict(self.engine.as_ref(), ps))
    }

    /// Borrow the installed engine.
    #[cfg(test)]
    pub(super) fn engine(&self) -> Option<&PolicyEngine> {
        self.engine.as_ref()
    }

    /// The compiled gate table.
    #[inline]
    pub(super) const fn gates(&self) -> PolicyGates {
        self.gates
    }

    /// Debit a named rate-limit bucket ([`PolicyEngine::evaluate`]).
    #[inline]
    pub(super) fn rate_limit_try_consume<T: TimeSource>(
        &mut self,
        id: &str,
        amount: u64,
        clock: &T,
    ) -> Option<bool> {
        self.engine
            .as_mut()
            .map(|engine| engine.rate_limit_try_consume(id, amount, clock))
    }

    /// Debit the [`RESPONSE_LIMIT_ID`] bucket through its pre-resolved slot.
    #[inline]
    pub(super) fn response_rate_limit_try_consume<T: TimeSource>(
        &mut self,
        amount: u64,
        clock: &T,
    ) -> Option<bool> {
        let engine = self.engine.as_mut()?;
        Some(engine.rate_limit_try_consume_slot(self.response_limit, amount, clock))
    }

    /// Debit one program notification from the installed policy's [`NOTIFICATIONS_LIMIT_ID`]
    /// bucket, or `None` when no installed policy declares one.
    #[inline]
    pub(super) fn notification_limit_try_consume<T: TimeSource>(
        &mut self,
        clock: &T,
    ) -> Option<bool> {
        if self.notifications_limit == RateLimitSlot::UNDECLARED {
            return None;
        }
        let engine = self.engine.as_mut()?;
        Some(engine.rate_limit_try_consume_slot(self.notifications_limit, 1, clock))
    }

    /// Look up a rate-limit configuration in the installed policy.
    #[inline]
    pub(super) fn rate_limit_config(&self, id: &str) -> Option<&RateLimit> {
        self.engine.as_ref()?.rate_limit_config(id)
    }

    /// Borrow the installed policy document, if any.
    #[inline]
    pub(super) fn policy_doc(&self) -> Option<&aterm_policy::Policy> {
        self.engine.as_ref().map(PolicyEngine::policy)
    }

    /// Snapshot declared rate-limit buckets, or empty when no policy is installed.
    pub(super) fn bucket_snapshots(&self) -> Vec<aterm_policy::limits::BucketSnapshot> {
        self.engine
            .as_ref()
            .map_or_else(Vec::new, PolicyEngine::bucket_snapshots)
    }

    /// Restore declared rate-limit buckets onto the installed policy engine, if any.
    pub(super) fn restore_bucket_snapshots(
        &mut self,
        snaps: &[aterm_policy::limits::BucketSnapshot],
    ) {
        if let Some(engine) = &mut self.engine {
            engine.restore_bucket_snapshots(snaps);
        }
    }

    /// Earliest `last_refill` instant across the installed policy's buckets, if any.
    pub(super) fn earliest_refill(&self) -> Option<aterm_time::Instant> {
        self.engine.as_ref().and_then(PolicyEngine::earliest_refill)
    }
}

fn compile_xtwinops(engine: Option<&PolicyEngine>) -> [BridgeDecision; XTWINOPS_GATES] {
    std::array::from_fn(|ps| {
        let ps = u16::try_from(ps).unwrap_or(XTWINOPS_MAX_PS);
        xtwinops_verdict(engine, ps)
    })
}

fn compile_shell(
    engine: Option<&PolicyEngine>,
) -> [[BridgeDecision; SHELL_SUBCOMMANDS]; SHELL_MAJORS.len()] {
    std::array::from_fn(|major| {
        std::array::from_fn(|sub| {
            let letter = char::from(b'A'.saturating_add(u8::try_from(sub).unwrap_or(0)));
            let mut buf = [0u8; 4];
            shell_verdict(engine, SHELL_MAJORS[major], letter.encode_utf8(&mut buf))
        })
    })
}

impl super::checkpoint_heap::Heap for PolicyState {
    fn heap(&self) -> usize {
        let Self {
            engine,
            gates: _,
            xtwinops: _,
            shell: _,
            response_limit: _,
            notifications_limit: _,
        } = self;
        const _: () = assert!(
            !std::mem::needs_drop::<(
                PolicyGates,
                [BridgeDecision; XTWINOPS_GATES],
                [[BridgeDecision; SHELL_SUBCOMMANDS]; SHELL_MAJORS.len()],
                RateLimitSlot,
            )>(),
            "a policy gate with drop glue holds heap: count it"
        );
        engine.heap()
    }
}

impl Default for PolicyState {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "policy_gates_tests.rs"]
mod tests;
