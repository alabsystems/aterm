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
        }
    }

    /// Install (or replace) the engine, recompiling every gate table.
    pub(super) fn install(&mut self, engine: PolicyEngine) {
        self.gates = PolicyGates::compile(Some(&engine));
        self.xtwinops = compile_xtwinops(Some(&engine));
        self.shell = compile_shell(Some(&engine));
        self.response_limit = engine.rate_limit_slot(RESPONSE_LIMIT_ID);
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

impl Default for PolicyState {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::Terminal;
    use aterm_policy::{Policy, Response, profiles};

    fn wildcard_policy(response: Response) -> Policy {
        crate::terminal::policy_bridge::one_rule_policy(
            "*",
            response,
            OriginTag::NetworkUntrusted,
            Response::Warn,
        )
    }

    fn policy_with_rule(sequence: &str, response: Response) -> Policy {
        crate::terminal::policy_bridge::one_rule_policy(
            sequence,
            response,
            OriginTag::Pty,
            Response::Warn,
        )
    }

    fn engine(response: Response) -> PolicyEngine {
        PolicyEngine::new(wildcard_policy(response))
    }

    fn replies(term: &mut Terminal) -> bool {
        term.process(b"\x1b[6n");
        term.take_response().is_some_and(|r| !r.is_empty())
    }

    #[test]
    fn compiled_gate_matches_live_evaluation_for_every_builtin_profile() {
        for policy in [
            profiles::permissive(),
            profiles::standard(),
            profiles::hardened(),
            wildcard_policy(Response::Drop),
            wildcard_policy(Response::Execute),
            wildcard_policy(Response::Warn),
        ] {
            let eng = PolicyEngine::new(policy);
            let probe = super::super::response_capability::probe_for(ProbeKind::response_sink());
            let live = policy_bridge::engine_decision(Some(&eng), &probe, GATE_ORIGIN);
            let gates = PolicyGates::compile(Some(&eng));
            assert_eq!(
                gates.response_sink(),
                live,
                "the compiled response-sink verdict must equal what the removed \
                 per-dispatch evaluation would have returned"
            );
            assert_eq!(
                gates.osc52_set(),
                policy_bridge::engine_decision_deny_by_default_capability(
                    Some(&eng),
                    &super::super::clipboard_auth::probe_osc52_set(),
                    GATE_ORIGIN,
                ),
                "compiled OSC 52 set verdict must equal the removed per-dispatch one"
            );
            assert_eq!(
                gates.osc52_query(),
                policy_bridge::engine_decision_deny_by_default_capability(
                    Some(&eng),
                    &super::super::clipboard_auth::probe_osc52_query(),
                    GATE_ORIGIN,
                ),
                "compiled OSC 52 query verdict must equal the removed per-dispatch one"
            );
        }
    }

    #[test]
    fn compiled_clipboard_gates_stay_deny_by_default_under_a_wildcard_allow() {
        let gates = PolicyGates::compile(Some(&engine(Response::Execute)));
        assert_eq!(
            gates.osc52_set(),
            BridgeDecision::Fallback,
            "a universal `* = Execute` rule must be demoted to Fallback at the \
             clipboard sinks, leaving the legacy authorization bit authoritative"
        );
        assert_eq!(gates.osc52_query(), BridgeDecision::Fallback);
        assert_eq!(
            gates.response_sink(),
            BridgeDecision::Allow,
            "the response sink is NOT deny-by-default: a wildcard Execute allows"
        );
    }

    #[test]
    fn clipboard_gate_tracks_the_installed_policy() {
        use crate::terminal::{ClipboardAccess, ClipboardOperation};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let hits = Arc::new(AtomicUsize::new(0));
        let sink = Arc::clone(&hits);
        let mut term = Terminal::new(24, 80);
        term.authorize_clipboard_access(ClipboardAccess::Write);
        term.set_clipboard_callback(move |op| {
            if matches!(op, ClipboardOperation::Set { .. }) {
                sink.fetch_add(1, Ordering::Relaxed);
            }
            None
        });

        let sets = |term: &mut Terminal| {
            let before = hits.load(Ordering::Relaxed);
            term.process(b"\x1b]52;c;SGVsbG8=\x07");
            hits.load(Ordering::Relaxed) > before
        };

        assert!(sets(&mut term), "no engine: the legacy write bit allows");

        term.apply_policy_engine(PolicyEngine::new(policy_with_rule(
            "OSC 52 set",
            Response::Drop,
        )));
        assert!(
            !sets(&mut term),
            "installing `OSC 52 set = Drop` must shut the write gate; a stale \
             memo would let PTY bytes reach the host clipboard under a policy \
             that forbids it"
        );

        term.apply_policy_engine(PolicyEngine::new(policy_with_rule(
            "OSC 52 set",
            Response::Execute,
        )));
        assert!(
            sets(&mut term),
            "swapping to `OSC 52 set = Execute` must reopen it"
        );

        term.apply_policy_engine(PolicyEngine::new(policy_with_rule(
            "OSC 52 set",
            Response::Drop,
        )));
        assert!(!sets(&mut term), "and swapping back must shut it again");
    }

    #[test]
    fn compiled_gate_matches_the_per_dispatch_mint() {
        use crate::terminal::response_capability::ResponseCapability;

        for policy in [
            profiles::permissive(),
            profiles::standard(),
            profiles::hardened(),
            wildcard_policy(Response::Drop),
            wildcard_policy(Response::Execute),
            wildcard_policy(Response::Ask),
        ] {
            let eng = PolicyEngine::new(policy);
            let minted = ResponseCapability::mint_for_dispatch_with_engine(
                Some(&eng),
                GATE_ORIGIN,
                ProbeKind::response_sink(),
            )
            .is_some();
            let compiled = PolicyGates::compile(Some(&eng))
                .response_sink()
                .resolve(true);
            assert_eq!(
                compiled, minted,
                "compiled response-sink gate disagrees with the per-dispatch mint"
            );
        }

        assert_eq!(
            PolicyGates::compile(None).response_sink().resolve(true),
            ResponseCapability::mint_for_dispatch_with_engine(
                None,
                GATE_ORIGIN,
                ProbeKind::response_sink(),
            )
            .is_some(),
        );
    }

    #[test]
    fn replacing_the_engine_invalidates_the_memo_in_both_directions() {
        let mut term = Terminal::new(24, 80);

        term.apply_policy_engine(engine(Response::Drop));
        assert!(!replies(&mut term), "Drop policy suppresses");

        term.apply_policy_engine(engine(Response::Execute));
        assert!(
            replies(&mut term),
            "swapping Drop -> Execute must lift the suppression; a stale memo \
             would keep answering Drop"
        );

        term.apply_policy_engine(engine(Response::Drop));
        assert!(
            !replies(&mut term),
            "swapping Execute -> Drop must re-apply it; a stale memo would keep \
             answering Execute and leak a reply the installed policy forbids"
        );
    }

    #[test]
    fn an_installed_policy_costs_no_per_dispatch_evaluation_at_the_compiled_gates() {
        const NONCE: [u8; 32] = [0x5A; 32];
        let id: String = NONCE.iter().fold(String::from("id="), |mut acc, b| {
            use std::fmt::Write as _;
            let _ = write!(acc, "{b:02x}");
            acc
        });
        let nonced_mark = format!("\x1b]133;A;{id}\x07").into_bytes();

        let mut term = Terminal::new(24, 80);
        term.authorize_shell_integration(NONCE);
        term.set_require_shell_integration_nonce(true);
        term.apply_policy_engine(PolicyEngine::new(profiles::standard()));
        let compiled = crate::terminal::policy_bridge::take_engine_decisions();
        assert!(
            compiled > 0,
            "installing a policy compiled no gate at all — the fixture is not \
             reaching the bridge and every zero below would be vacuous"
        );

        const BURST: usize = 32;
        for _ in 0..BURST {
            term.process(b"\x1b[6n");
            let _ = term.take_response();
            term.process(b"\x1b[c");
            let _ = term.take_response();
            term.process(b"\x1b]52;c;?\x07");
            term.process(b"\x1b]52;c;aGk=\x07");
            let _ = term.take_response();
            term.process(&nonced_mark);
            term.process(b"\x1b[18t");
            let _ = term.take_response();
        }
        assert_eq!(
            term.shell_integration_dropped_count(),
            0,
            "the burst's nonced mark was dropped — it is not reaching the \
             shell-integration gate, so AMB-3 is uncovered again"
        );
        assert_eq!(
            crate::terminal::policy_bridge::take_engine_decisions(),
            0,
            "{BURST} rounds of covered sequences re-entered the policy bridge. \
             The fixed capability gates are supposed to be COMPILED once per \
             policy generation and read as a byte at dispatch; this is the \
             per-event probe-and-walk the campaign removed, back again. No \
             verdict changed, so nothing else in this file can see it."
        );

        term.process(format!("\x1b[{}t", XTWINOPS_MAX_PS + 1).as_bytes());
        let _ = term.take_response();
        assert!(
            crate::terminal::policy_bridge::take_engine_decisions() > 0,
            "an out-of-range XTWINOPS Ps did not reach the bridge — the counter \
             is not reading the live-evaluation path, so the zero above proves \
             nothing"
        );

        term.process(format!("\x1b]133;z;{id}\x07").as_bytes());
        let _ = term.take_response();
        assert!(
            crate::terminal::policy_bridge::take_engine_decisions() > 0,
            "an uncompiled OSC 133 subcommand did not reach the bridge — the \
             shell-integration gate is not being consulted at all (the \
             nonce-required mode bit is the usual reason), so the zero above is \
             vacuous for AMB-3"
        );
    }

    #[test]
    fn standard_profile_keeps_replying() {
        let mut term = Terminal::new(24, 80);
        term.apply_policy_engine(PolicyEngine::new(profiles::standard()));
        assert!(
            replies(&mut term),
            "the shipping profile allows the response sink; the compiled gate \
             must not tighten it"
        );
    }

    #[test]
    fn xtwinops_table_matches_live_evaluation_for_every_covered_ps() {
        for policy in [
            profiles::standard(),
            profiles::hardened(),
            policy_with_rule("CSI 20 t", Response::Execute),
            policy_with_rule("CSI t", Response::Drop),
            wildcard_policy(Response::Execute),
        ] {
            let mut state = PolicyState::new();
            state.install(PolicyEngine::new(policy));
            for ps in 0..=XTWINOPS_MAX_PS {
                assert_eq!(
                    state.xtwinops_gate(ps),
                    xtwinops_verdict(state.engine(), ps),
                    "compiled XTWINOPS slot {ps} disagrees with a live evaluation"
                );
            }
        }

        let mut state = PolicyState::new();
        state.install(PolicyEngine::new(policy_with_rule(
            "CSI 20 t",
            Response::Execute,
        )));
        assert_eq!(state.xtwinops_gate(20), BridgeDecision::Allow);
        assert_eq!(state.xtwinops_gate(21), BridgeDecision::Fallback);
    }

    #[test]
    fn xtwinops_out_of_range_ps_falls_back_to_live_evaluation() {
        let mut state = PolicyState::new();
        state.install(PolicyEngine::new(policy_with_rule(
            "CSI 999 t",
            Response::Execute,
        )));

        assert_eq!(
            state.xtwinops_gate(999),
            BridgeDecision::Allow,
            "a rule naming a major outside the compiled range must still be seen"
        );
        assert_eq!(
            state.xtwinops_gate(u16::MAX),
            BridgeDecision::Fallback,
            "and an unnamed out-of-range major must fall through, not inherit \
             the previous out-of-range answer"
        );
        assert_eq!(
            state.xtwinops_gate(18),
            BridgeDecision::Fallback,
            "an in-range Ps must not be widened by an out-of-range rule"
        );
    }

    #[test]
    fn xtwinops_gate_tracks_the_installed_policy() {
        let reports = |term: &mut Terminal| {
            term.process(b"\x1b[18t");
            term.take_response().is_some_and(|r| !r.is_empty())
        };

        let mut term = Terminal::new(24, 80);
        assert!(
            !reports(&mut term),
            "no engine, legacy bool false: no report"
        );

        term.apply_policy_engine(PolicyEngine::new(policy_with_rule(
            "CSI t",
            Response::Execute,
        )));
        assert!(
            reports(&mut term),
            "installing `CSI t = Execute` must open the gate immediately"
        );

        term.apply_policy_engine(PolicyEngine::new(policy_with_rule("CSI t", Response::Drop)));
        assert!(
            !reports(&mut term),
            "swapping to Drop must shut it; a stale memo would keep leaking \
             window geometry to a PTY the installed policy forbids"
        );

        term.apply_policy_engine(PolicyEngine::new(policy_with_rule(
            "CSI t",
            Response::Execute,
        )));
        assert!(reports(&mut term), "and swapping back must reopen it");
    }

    #[test]
    fn shell_table_matches_live_evaluation_for_every_covered_pair() {
        for policy in [
            profiles::standard(),
            profiles::hardened(),
            policy_with_rule("OSC 133", Response::Drop),
            policy_with_rule("OSC 133;C", Response::Drop),
            wildcard_policy(Response::Drop),
        ] {
            let mut state = PolicyState::new();
            state.install(PolicyEngine::new(policy));
            for &major in &SHELL_MAJORS {
                for letter in b'A'..=b'Z' {
                    let sub = String::from(char::from(letter));
                    let params: [&[u8]; 2] = [b"133", sub.as_bytes()];
                    assert_eq!(
                        state.shell_integration_gate(major, &params),
                        shell_verdict(state.engine(), major, &sub),
                        "compiled shell slot ({major}, {sub}) disagrees with a live \
                         evaluation"
                    );
                }
            }
        }
    }

    #[test]
    fn shell_gate_stays_subcommand_sensitive() {
        let mut state = PolicyState::new();
        state.install(PolicyEngine::new(policy_with_rule(
            "OSC 133;C",
            Response::Drop,
        )));

        let c: [&[u8]; 2] = [b"133", b"C"];
        let a: [&[u8]; 2] = [b"133", b"A"];
        assert_eq!(
            state.shell_integration_gate(133, &c),
            BridgeDecision::Deny,
            "`OSC 133;C = Drop` must deny the C mark"
        );
        assert_eq!(
            state.shell_integration_gate(133, &a),
            BridgeDecision::Fallback,
            "...and must NOT deny the A mark — a memo keyed only on the major \
             would answer Deny here and silently kill prompt marks"
        );
    }

    #[test]
    fn shell_gate_falls_back_for_uncovered_subcommands() {
        let mut state = PolicyState::new();
        state.install(PolicyEngine::new(policy_with_rule(
            "OSC 133;lower",
            Response::Drop,
        )));

        let lower: [&[u8]; 2] = [b"133", b"lower"];
        let upper: [&[u8]; 2] = [b"133", b"A"];
        assert_eq!(
            state.shell_integration_gate(133, &lower),
            BridgeDecision::Deny,
            "a multi-byte subcommand must reach a live evaluation"
        );
        assert_eq!(
            state.shell_integration_gate(133, &upper),
            BridgeDecision::Fallback
        );

        let mut state = PolicyState::new();
        state.install(PolicyEngine::new(policy_with_rule(
            "OSC 1337",
            Response::Drop,
        )));
        let a: [&[u8]; 2] = [b"1337", b"A"];
        assert_eq!(state.shell_integration_gate(1337, &a), BridgeDecision::Deny);
    }

    #[test]
    fn shell_gate_treats_an_absent_subcommand_as_a() {
        let mut state = PolicyState::new();
        state.install(PolicyEngine::new(policy_with_rule(
            "OSC 133;A",
            Response::Drop,
        )));
        let bare: [&[u8]; 1] = [b"133"];
        assert_eq!(
            state.shell_integration_gate(133, &bare),
            BridgeDecision::Deny,
            "a bare `OSC 133` is probed as an A mark; the table must match the \
             probe, quirk and all"
        );
    }

    #[test]
    fn shell_gate_tracks_the_installed_policy() {
        const NONCE: [u8; 32] = [0x5A; 32];
        let id: String = NONCE.iter().fold(String::from("id="), |mut acc, b| {
            use std::fmt::Write as _;
            let _ = write!(acc, "{b:02x}");
            acc
        });
        let mark = format!("\x1b]133;A;{id}\x07").into_bytes();

        let mut term = Terminal::new(24, 80);
        term.authorize_shell_integration(NONCE);
        term.set_require_shell_integration_nonce(true);

        let dropped = |term: &mut Terminal, bytes: &[u8]| {
            let before = term.shell_integration_dropped_count();
            term.process(bytes);
            term.shell_integration_dropped_count() > before
        };

        assert!(
            !dropped(&mut term, &mark),
            "no engine: a correctly nonced mark passes"
        );

        term.apply_policy_engine(PolicyEngine::new(policy_with_rule(
            "OSC 133",
            Response::Drop,
        )));
        assert!(
            dropped(&mut term, &mark),
            "installing `OSC 133 = Drop` must deny the mark immediately"
        );

        term.apply_policy_engine(PolicyEngine::new(profiles::standard()));
        assert!(
            !dropped(&mut term, &mark),
            "swapping to a profile with no OSC 133 rule must lift the denial; a \
             stale memo would keep dropping every prompt mark"
        );
    }

    #[test]
    fn rate_limit_passthrough_reports_absence_of_an_engine() {
        let mut state = PolicyState::new();
        let clock = crate::terminal::response_rate_limiter::BatchClock(aterm_time::Instant::now());
        assert_eq!(
            state.rate_limit_try_consume("response", 1, &clock),
            None,
            "no engine installed: the caller must be told so it can use its \
             legacy limiter, not silently allowed"
        );

        state.install(PolicyEngine::new(profiles::standard()));
        assert_eq!(
            state.rate_limit_try_consume("response", 1, &clock),
            Some(true),
            "a fresh standard-profile `response` bucket permits a 1-byte debit"
        );
        assert_eq!(
            state.rate_limit_try_consume("no-such-bucket", 1, &clock),
            Some(true),
            "unknown ids stay permitted (limits.rs: \"unknown id => allow\" is \
             deliberate) — the compiled table must not change that"
        );
    }

    fn policy_with_limits(limits: &[(&str, u32)]) -> Policy {
        Policy {
            rate_limits: limits
                .iter()
                .map(|&(id, capacity_bytes)| RateLimit {
                    id: id.to_owned(),
                    capacity_bytes,
                    refill_per_second: 0,
                    per_sequence_max: 0,
                })
                .collect(),
            ..policy_with_rule("*", Response::Execute)
        }
    }

    #[test]
    fn response_slot_answers_exactly_as_the_id_does() {
        let clock = crate::terminal::response_rate_limiter::BatchClock(aterm_time::Instant::now());
        let mut by_id = PolicyState::new();
        let mut by_slot = PolicyState::new();
        by_id.install(PolicyEngine::new(profiles::standard()));
        by_slot.install(PolicyEngine::new(profiles::standard()));
        for step in 0..12 {
            let amount = 8 * 1024;
            assert_eq!(
                by_id.rate_limit_try_consume(RESPONSE_LIMIT_ID, amount, &clock),
                by_slot.response_rate_limit_try_consume(amount, &clock),
                "slot and id forms disagreed at step {step}"
            );
        }
    }

    #[test]
    fn response_slot_allows_when_the_policy_declares_no_response_bucket() {
        let mut state = PolicyState::new();
        let clock = crate::terminal::response_rate_limiter::BatchClock(aterm_time::Instant::now());
        state.install(PolicyEngine::new(policy_with_limits(&[("palette", 0)])));
        assert_eq!(
            state.response_rate_limit_try_consume(u64::MAX, &clock),
            Some(true),
            "the policy declares no `response` bucket, so the slot is UNDECLARED \
             and the debit allows — the same \"unknown id => allow\" answer the \
             string form gives. Resolving early must not turn an undeclared id \
             into a denial, nor into a debit of somebody else's bucket"
        );
    }

    #[test]
    fn response_slot_follows_the_id_not_the_declaration_order() {
        let clock = crate::terminal::response_rate_limiter::BatchClock(aterm_time::Instant::now());
        let mut state = PolicyState::new();
        state.install(PolicyEngine::new(policy_with_limits(&[
            ("palette", 0),
            (RESPONSE_LIMIT_ID, 4096),
        ])));
        assert_eq!(
            state.response_rate_limit_try_consume(4096, &clock),
            Some(true),
            "the slot must name the `response` bucket, not slot 0"
        );
        assert_eq!(
            state.response_rate_limit_try_consume(1, &clock),
            Some(false),
            "and it must be the bucket that was just drained"
        );
    }

    #[test]
    fn response_slot_tracks_the_installed_policy() {
        let mut term = Terminal::new(24, 80);
        assert!(replies(&mut term), "no engine: the legacy limiter allows");

        term.apply_policy_engine(PolicyEngine::new(policy_with_limits(&[(
            RESPONSE_LIMIT_ID,
            0,
        )])));
        assert!(
            !replies(&mut term),
            "installing a zero-capacity `response` bucket must suppress the reply \
             immediately"
        );

        term.apply_policy_engine(PolicyEngine::new(policy_with_limits(&[
            ("clipboard", 0),
            (RESPONSE_LIMIT_ID, 64 * 1024),
        ])));
        assert!(
            replies(&mut term),
            "swapping policies must re-resolve the slot; a stale one would still \
             point at the old bucket (and here, at a zero-capacity `clipboard`)"
        );

        term.apply_policy_engine(PolicyEngine::new(policy_with_limits(&[(
            RESPONSE_LIMIT_ID,
            0,
        )])));
        assert!(
            !replies(&mut term),
            "re-installing the hard block must bite"
        );
    }
}
