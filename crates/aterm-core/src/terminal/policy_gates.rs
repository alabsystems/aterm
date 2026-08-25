// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Compiled per-gate policy decisions, resolved once per installed policy.
//!
//! # The waste this removes
//!
//! Several capability gates in `terminal/` consult the [`PolicyEngine`] once per
//! dispatched sequence with a probe whose every input is a **compile-time
//! constant**. The response sink is the clearest case: `send_response` builds
//! `ProbeKind::response_sink()` — the literal `Dcs { final_byte: 0 }` — at the
//! literal origin [`OriginTag::Pty`], turns it into two heap `String`
//! allocations, and walks the rule set, on **every reply**. Nothing in that
//! computation varies between two calls under the same policy, so the answer can
//! only change when a policy is installed, replaced, or cleared.
//!
//! [`PolicyGates`] resolves those constant probes once, at policy-install time,
//! into a table of [`BridgeDecision`]s. The per-dispatch cost becomes a field
//! read: no allocation, no hash lookup, no rule walk.
//!
//! # Why this cannot go stale
//!
//! A stale memo here would be a security defect, not a performance one: a gate
//! could answer with a verdict from a policy that is no longer installed. The
//! table is therefore **not** a field sitting next to the engine that callers
//! must remember to refresh. [`PolicyState`] owns both, its fields are private
//! to this module, and it exposes exactly two mutators —
//! [`PolicyState::install`] and [`PolicyState::clear`] — each of which
//! recompiles the table in the same statement that changes the engine. There is
//! no way to reach the engine mutably from outside this module (in particular
//! `PolicyEngine::replace_policy` is unreachable through this type), so "the
//! engine changed but the table did not" is not a representable state. That is
//! the invariant the tests at the bottom of this file pin from the outside, by
//! driving a real `Terminal` through every install/replace/clear order.
//!
//! # Why the memo is exactly equivalent
//!
//! [`PolicyEngine::evaluate`] takes `&self` and is side-effect-free: it reads
//! the compiled rule set and returns a [`aterm_policy::Decision`]. It is a pure
//! function of (policy, sequence, origin). Every gate compiled here holds
//! sequence and origin fixed, and the policy is fixed for the lifetime of the
//! table. Each gate's probe is built by the owning capability module's single
//! probe constructor — the SAME function the per-dispatch path called — so the
//! compiled verdict is by construction the verdict the removed call would have
//! produced. A gate whose probe is not fully constant (XTWINOPS varies with
//! `Ps`, shell integration with the subcommand) is keyed on the varying part,
//! and any key outside the compiled range falls back to a live evaluation rather
//! than being answered from a slot it does not own.

use aterm_policy::engine::PolicyEngine;
use aterm_policy::limits::{RateLimitSlot, TimeSource};
use aterm_policy::{OriginTag, RateLimit};

use super::policy_bridge::{self, BridgeDecision};
use super::response_capability::ProbeKind;

/// The origin every compiled gate is resolved at.
///
/// This is a call-site fact, not a type guarantee: every production gate site
/// passes the literal `OriginTag::Pty`. Compiling a gate at a fixed origin is
/// only sound while that stays true, so the sites read this constant instead of
/// spelling `Pty` again.
pub(super) const GATE_ORIGIN: OriginTag = OriginTag::Pty;

/// Largest XTWINOPS `Ps` the compiled table covers, inclusive.
///
/// xterm defines 1..=24 and `Ps` absent means 0, so this covers the whole
/// defined opcode space. It deliberately does NOT cover the rest of the `u16`:
/// `Ps` comes straight off the wire (`params.first().unwrap_or(0)`) and is
/// attacker-controlled over the full range, while a policy may legitimately name
/// any major (`"CSI 999 t"` parses fine). Answering an out-of-range `Ps` from a
/// shared slot would therefore be wrong, so
/// [`PolicyState::xtwinops_gate`] falls back to a live evaluation there.
const XTWINOPS_MAX_PS: u16 = 24;

/// The rate-limit id the response sink debits, once per reply.
///
/// Named here rather than spelled at the call site because [`PolicyState`]
/// resolves it to a [`RateLimitSlot`] at install time and `send_response` then
/// debits that slot — the id string and its resolution must not drift apart.
pub(super) const RESPONSE_LIMIT_ID: &str = "response";

/// Compiled XTWINOPS slots: `Ps` 0..=[`XTWINOPS_MAX_PS`].
const XTWINOPS_GATES: usize = XTWINOPS_MAX_PS as usize + 1;

/// OSC majors the shell-integration gate covers, in table order.
const SHELL_MAJORS: [u32; 2] = [133, 633];

/// Subcommand letters the shell-integration table covers: `A`..=`Z`.
///
/// The probe's subcommand is `params[1]` (or a fabricated `"A"`), and an OSC
/// param selector CAN discriminate on it (`"OSC 133;C"` parses), so the table is
/// keyed on the subcommand and not on the major alone. Every mark the shipped
/// preambles emit — 133 `A`/`B`/`C`/`D`, 633 `A`..`H`/`P` — is a single ASCII
/// uppercase letter; anything else (a multi-byte subcommand, a lowercase one,
/// non-UTF-8) is evaluated live rather than answered from a shared slot.
const SHELL_SUBCOMMANDS: usize = 26;

/// The shell-integration gate verdict for one `(major, subcommand)`, live.
#[must_use]
fn shell_verdict(engine: Option<&PolicyEngine>, command: u32, subcommand: &str) -> BridgeDecision {
    policy_bridge::engine_decision(
        engine,
        &super::shell_integration_auth::probe_shell_integration(command, subcommand),
        GATE_ORIGIN,
    )
}

/// Table coordinates for a `(major, subcommand)` pair, or `None` when the pair
/// is outside the compiled range and must be evaluated live.
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

/// The XTWINOPS gate verdict for one `Ps`, evaluated live.
///
/// The single definition of that decision: [`PolicyState`] calls it once per
/// covered `Ps` at install time and again on the out-of-range fallback path, and
/// `window_auth`'s tests call it to check the capability mint against the same
/// verdict production reads.
#[must_use]
pub(super) fn xtwinops_verdict(engine: Option<&PolicyEngine>, ps: u16) -> BridgeDecision {
    policy_bridge::engine_decision_deny_by_default_capability(
        engine,
        &super::window_auth::probe_xtwinops(ps),
        GATE_ORIGIN,
    )
}

/// Policy verdicts for the fixed capability gates, resolved once per policy.
///
/// One byte per gate. Copied out by value at the dispatch sites so a gate read
/// never borrows the terminal.
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
    /// Resolve every gate against `engine` (or against "no engine installed",
    /// which every gate answers as [`BridgeDecision::Fallback`]).
    ///
    /// `pub(super)` so sibling modules' tests can build a table directly from an
    /// engine and compare it against the per-dispatch path they replaced.
    pub(super) fn compile(engine: Option<&PolicyEngine>) -> Self {
        Self {
            response_sink: policy_bridge::engine_decision(
                engine,
                &super::response_capability::probe_for(ProbeKind::response_sink()),
                GATE_ORIGIN,
            ),
            // Deny-by-default capability sinks: a broad `response any = Execute`
            // must NOT reopen them, which is why these use the
            // wildcard-demoting bridge entry point (`policy_bridge` module docs).
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

    /// Verdict for the response sink. See [`BridgeDecision::resolve`] for how
    /// the caller folds it against its legacy boolean.
    pub(super) const fn response_sink(self) -> BridgeDecision {
        self.response_sink
    }

    /// Verdict for the OSC 52 clipboard *set* (and *clear*) gate.
    pub(super) const fn osc52_set(self) -> BridgeDecision {
        self.osc52_set
    }

    /// Verdict for the OSC 52 clipboard *query* gate.
    pub(super) const fn osc52_query(self) -> BridgeDecision {
        self.osc52_query
    }
}

/// The terminal's policy: the installed engine plus its compiled gate table.
///
/// The two are inseparable by construction — see the module docs.
#[derive(Debug)]
pub(super) struct PolicyState {
    /// The installed engine, or `None` when the host has not installed a policy.
    engine: Option<PolicyEngine>,
    /// Gate verdicts compiled from `engine`. Always in sync: the only writers
    /// are [`Self::install`] and [`Self::clear`].
    gates: PolicyGates,
    /// XTWINOPS verdicts for `Ps` 0..=[`XTWINOPS_MAX_PS`], compiled from
    /// `engine` by the same two writers.
    ///
    /// Kept out of [`PolicyGates`] on purpose: `PolicyGates` is copied by value
    /// at every gate read, and the response sink — the hottest of the gates —
    /// should not carry a 25-byte table past it.
    xtwinops: [BridgeDecision; XTWINOPS_GATES],
    /// OSC 133 / OSC 633 verdicts, indexed by [`SHELL_MAJORS`] then by
    /// subcommand letter. Compiled by the same two writers.
    shell: [[BridgeDecision; SHELL_SUBCOMMANDS]; SHELL_MAJORS.len()],
    /// The [`RESPONSE_LIMIT_ID`] bucket's slot in `engine`'s limiter set,
    /// resolved by the same two writers.
    ///
    /// The gate tables above removed the per-reply rule walk from
    /// `send_response`; this removes the other per-reply constant, the hash of
    /// the literal `"response"` into the limiter set on EVERY reply. Same
    /// staleness argument as the tables: the slot is recompiled in the
    /// statement that changes the engine, so it always names a bucket of the
    /// engine actually installed. `RateLimitSlot::UNDECLARED` (the [`Default`])
    /// is the correct value with no engine, and also for a policy that
    /// declares no `"response"` limit — both mean "unlimited".
    response_limit: RateLimitSlot,
}

impl PolicyState {
    /// No policy installed — the pre-`apply_policy_engine` default.
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

    /// Drop the engine, recompiling every gate table back to the legacy posture.
    pub(super) fn clear(&mut self) {
        self.engine = None;
        self.gates = PolicyGates::compile(None);
        self.xtwinops = compile_xtwinops(None);
        self.shell = compile_shell(None);
        self.response_limit = RateLimitSlot::UNDECLARED;
    }

    /// OSC 133 / OSC 633 gate verdict for this dispatch.
    ///
    /// O(1) table read for the `(major, subcommand-letter)` pairs shell
    /// integration actually uses; a live evaluation — which allocates, exactly
    /// as the old per-mark path always did — for anything outside it.
    pub(super) fn shell_integration_gate(&self, command: u32, params: &[&[u8]]) -> BridgeDecision {
        let subcommand = super::shell_integration_auth::probe_subcommand(params);
        shell_slot(command, subcommand).map_or_else(
            || shell_verdict(self.engine.as_ref(), command, subcommand),
            |(major, sub)| self.shell[major][sub],
        )
    }

    /// XTWINOPS verdict for `ps`.
    ///
    /// O(1) table read inside the compiled range; a live evaluation outside it,
    /// because `ps` is attacker-controlled over the whole `u16` and a policy may
    /// name any major. See [`XTWINOPS_MAX_PS`].
    pub(super) fn xtwinops_gate(&self, ps: u16) -> BridgeDecision {
        self.xtwinops
            .get(usize::from(ps))
            .copied()
            .unwrap_or_else(|| xtwinops_verdict(self.engine.as_ref(), ps))
    }

    /// Borrow the installed engine. Shared borrow only — a caller can read the
    /// policy but cannot swap it behind the compiled table's back.
    pub(super) fn engine(&self) -> Option<&PolicyEngine> {
        self.engine.as_ref()
    }

    /// The compiled gate table.
    pub(super) const fn gates(&self) -> PolicyGates {
        self.gates
    }

    /// Debit a named rate-limit bucket, or `None` when no engine is installed
    /// (the caller then uses its legacy limiter).
    ///
    /// Bucket state is the one thing a dispatch legitimately mutates in the
    /// engine, and it cannot affect any gate verdict — [`PolicyEngine::evaluate`]
    /// never reads the limiters. This is why `PolicyState` can hand out this
    /// mutation while never handing out `&mut PolicyEngine`.
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

    /// Debit the [`RESPONSE_LIMIT_ID`] bucket through its pre-resolved slot —
    /// what [`Self::rate_limit_try_consume`] would answer for that id, without
    /// hashing the literal on every reply.
    ///
    /// `None` still means "no engine installed", so `send_response` keeps
    /// falling back to its legacy in-transient limiter unchanged.
    ///
    /// The engine is tested BEFORE the slot is read (disjoint field borrows
    /// make that spelling legal), so the no-engine path does exactly the work
    /// it did before this cache existed and cannot pay for a field it will not
    /// use.
    pub(super) fn response_rate_limit_try_consume<T: TimeSource>(
        &mut self,
        amount: u64,
        clock: &T,
    ) -> Option<bool> {
        let engine = self.engine.as_mut()?;
        Some(engine.rate_limit_try_consume_slot(self.response_limit, amount, clock))
    }

    /// Look up a rate-limit configuration in the installed policy.
    pub(super) fn rate_limit_config(&self, id: &str) -> Option<&RateLimit> {
        self.engine.as_ref()?.rate_limit_config(id)
    }
}

/// Resolve the XTWINOPS verdict for every covered `Ps` in one pass.
fn compile_xtwinops(engine: Option<&PolicyEngine>) -> [BridgeDecision; XTWINOPS_GATES] {
    std::array::from_fn(|ps| {
        let ps = u16::try_from(ps).unwrap_or(XTWINOPS_MAX_PS);
        xtwinops_verdict(engine, ps)
    })
}

/// Resolve the shell-integration verdict for every covered pair in one pass.
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
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::Terminal;
    use aterm_policy::{Defaults, Policy, Profile, Response, Rule, SCHEMA_VERSION, profiles};

    /// A policy whose single `*` rule gives `response` to every sequence from
    /// any origin — the coarsest way to flip the response-sink gate.
    fn wildcard_policy(response: Response) -> Policy {
        Policy {
            schema_version: SCHEMA_VERSION,
            profile: Profile::Standard,
            defaults: Defaults {
                unmatched: Response::Warn,
                shell_integration_require_nonce: false,
            },
            rules: vec![Rule {
                sequence: "*".to_owned(),
                origin_min: OriginTag::NetworkUntrusted,
                response,
                rate_limit: None,
                prompt_id: None,
            }],
            rate_limits: vec![],
        }
    }

    /// A policy with one rule on an arbitrary selector, at an origin `Pty`
    /// dominates.
    fn policy_with_rule(sequence: &str, response: Response) -> Policy {
        Policy {
            schema_version: SCHEMA_VERSION,
            profile: Profile::Standard,
            defaults: Defaults {
                unmatched: Response::Warn,
                shell_integration_require_nonce: false,
            },
            rules: vec![Rule {
                sequence: sequence.to_owned(),
                origin_min: OriginTag::Pty,
                response,
                rate_limit: None,
                prompt_id: None,
            }],
            rate_limits: vec![],
        }
    }

    fn engine(response: Response) -> PolicyEngine {
        PolicyEngine::new(wildcard_policy(response))
    }

    /// Drive one CPR request and report whether the terminal replied.
    ///
    /// This is the OBSERVABLE the staleness tests hang on: `\x1b[6n` reaches
    /// `send_response`, which is the gate this table compiles. A stale table
    /// shows up here as a reply that should have been suppressed (or a
    /// suppression that should have been lifted).
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

    /// A `response any = Execute` wildcard must NOT reopen the deny-by-default
    /// clipboard sinks. This is the property `engine_decision_deny_by_default_
    /// capability` exists for, and compiling the verdict must not lose it.
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

    /// Staleness, observed through the clipboard delegate rather than the
    /// response buffer: a `OSC 52 set = Drop` policy must shut the write gate
    /// the instant it is installed and reopen it the instant it is replaced.
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

        term.clear_policy_engine();
        assert!(
            sets(&mut term),
            "clearing the engine must restore the legacy write bit"
        );
    }

    /// The compiled table must agree with the per-dispatch mint it replaced,
    /// for every profile and for policies on both sides of the decision.
    ///
    /// `mint_for_dispatch_with_engine` is the code that used to run on every
    /// reply; keeping it as the oracle means a future change to probe shape or
    /// to the bridge's wildcard-demotion rule that forgets this table fails
    /// here instead of silently answering a gate from the wrong rule.
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
            let compiled = PolicyGates::compile(Some(&eng)).response_sink().resolve(true);
            assert_eq!(
                compiled, minted,
                "compiled response-sink gate disagrees with the per-dispatch mint"
            );
        }

        // And with no engine at all, where the mint takes its own early return.
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
    fn no_engine_compiles_to_fallback() {
        assert_eq!(
            PolicyGates::compile(None).response_sink(),
            BridgeDecision::Fallback,
            "with no engine installed the gate must defer to the legacy path, \
             exactly as `engine_decision(None, ..)` does"
        );
    }

    #[test]
    fn apply_policy_engine_invalidates_the_memo() {
        let mut term = Terminal::new(24, 80);
        assert!(replies(&mut term), "no engine: the legacy path replies");

        term.apply_policy_engine(engine(Response::Drop));
        assert!(
            !replies(&mut term),
            "installing a `* = Drop` policy must suppress the reply — a stale \
             memo would answer from the previous (absent) policy"
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
    fn clear_policy_engine_invalidates_the_memo() {
        let mut term = Terminal::new(24, 80);

        term.apply_policy_engine(engine(Response::Drop));
        assert!(!replies(&mut term), "Drop policy suppresses");

        term.clear_policy_engine();
        assert!(
            replies(&mut term),
            "clearing the engine must restore the legacy path; a stale memo \
             would keep suppressing replies under no policy at all"
        );
    }


    /// AMB-1..4 AS A COUNT — the one thing the equivalence tests above cannot
    /// see.
    ///
    /// Every test in this module compares the compiled table's VERDICT against a
    /// live evaluation's verdict. That is the right correctness gate and it is
    /// exactly why none of them can notice the optimization going away: a gate
    /// site that went back to minting a probe and walking the rule set per
    /// dispatch would agree with the table on every input and pass all of them
    /// green, while a DSR/DA flood paid ~40-50 ns per reply again at full parser
    /// rate — before the rate-limit debit, so even replies that are then dropped
    /// pay it.
    ///
    /// So the count is the instrument: after a policy is installed, a burst of
    /// covered sequences must reach the bridge ZERO times. This is
    /// machine-independent and exact, unlike the timing lanes that measured the
    /// win (51.2 -> 11.3 ns per gate), and it rides `cargo test`.
    ///
    /// TWO-SIDED. An UNCOVERED sequence — an XTWINOPS `Ps` past the compiled
    /// range, and an OSC 133 subcommand outside the compiled letter table — must
    /// still be evaluated live, so the counter is proven to be reading the real
    /// path rather than a path the burst never entered. Without that half, a
    /// counter that had been accidentally disconnected would report zero and
    /// this test would celebrate.
    ///
    /// PER-KIND, NOT IN AGGREGATE. Five gate KINDS are compiled here
    /// (`response_sink`, `osc52_query`, `osc52_set`, the XTWINOPS table, the
    /// shell table) and each is read at its OWN dispatch site, so "delete the
    /// optimization and watch this go red" is only honest if it is checked ONE
    /// KIND AT A TIME. It was not, when this test landed: sending the SHELL gate
    /// alone back to a per-dispatch `shell_verdict` left this test GREEN,
    /// because `Terminal::new` leaves `require_shell_integration_nonce` FALSE
    /// and `shell_nonce_gate_ok` returns before it consults the gate at all —
    /// the burst's `OSC 133;A` never reached AMB-3. Hence the nonce arming
    /// below, which is load-bearing and not scene-setting.
    ///
    /// WHAT IT CANNOT CATCH: `evaluate` itself getting slower. Counts guard the
    /// structure of a win, never its constant factor.
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
        // ARM THE SHELL GATE. `shell_nonce_gate_ok` short-circuits on
        // `!require_shell_integration_nonce` — the default posture — and the
        // gate below it is then never read, so a burst against a bare terminal
        // measures NOTHING for AMB-3. The nonce is authorized too, so the mark
        // is a mark that really is processed rather than one dropped on the way.
        term.authorize_shell_integration(NONCE);
        term.set_require_shell_integration_nonce(true);
        term.apply_policy_engine(PolicyEngine::new(profiles::standard()));
        // The INSTALL itself compiles the whole table, which is the one place
        // the evaluation is supposed to happen. Discard that.
        let compiled = crate::terminal::policy_bridge::take_engine_decisions();
        assert!(
            compiled > 0,
            "installing a policy compiled no gate at all — the fixture is not \
             reaching the bridge and every zero below would be vacuous"
        );

        const BURST: usize = 32;
        for _ in 0..BURST {
            // The response sink (AMB-2): CPR, the single sink all 31
            // reply-producing sequences share.
            term.process(b"\x1b[6n");
            let _ = term.take_response();
            // Primary DA — the same sink, a different producer.
            term.process(b"\x1b[c");
            let _ = term.take_response();
            // OSC 52 query and set (AMB-1).
            term.process(b"\x1b]52;c;?\x07");
            term.process(b"\x1b]52;c;aGk=\x07");
            let _ = term.take_response();
            // A shell-integration mark (AMB-3) and an in-range CSI t (AMB-4).
            // NONCED: the gate sits UNDER the nonce-required check, so a bare
            // `OSC 133;A` against a default terminal never reaches it and this
            // line would cover nothing (see the per-kind note above).
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

        // THE OTHER SIDE: a `Ps` past the compiled range is documented to fall
        // back to a live evaluation, and must still do one.
        term.process(format!("\x1b[{}t", XTWINOPS_MAX_PS + 1).as_bytes());
        let _ = term.take_response();
        assert!(
            crate::terminal::policy_bridge::take_engine_decisions() > 0,
            "an out-of-range XTWINOPS Ps did not reach the bridge — the counter \
             is not reading the live-evaluation path, so the zero above proves \
             nothing"
        );

        // THE OTHER SIDE FOR THE SHELL KIND, which the XTWINOPS probe above
        // cannot stand in for: they are different tables read at different
        // dispatch sites, and this one sits under a mode bit that is off by
        // default. A LOWERCASE subcommand is outside `shell_slot`'s compiled
        // letters and is documented to fall back to a live evaluation, so a
        // zero here means the burst's mark never reached AMB-3 either and its
        // half of the count above proves nothing.
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

    /// Every covered `Ps` must read back exactly what a live evaluation says —
    /// including under a policy that discriminates BY `Ps`, which is the case a
    /// single shared slot would get wrong.
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

        // The discriminating case, spelled out: `CSI 20 t = Execute` must open
        // Ps 20 and ONLY Ps 20.
        let mut state = PolicyState::new();
        state.install(PolicyEngine::new(policy_with_rule("CSI 20 t", Response::Execute)));
        assert_eq!(state.xtwinops_gate(20), BridgeDecision::Allow);
        assert_eq!(state.xtwinops_gate(21), BridgeDecision::Fallback);
    }

    /// `Ps` is attacker-controlled over the whole `u16` and a policy may name any
    /// major, so an out-of-range `Ps` must be evaluated for real rather than
    /// answered from a shared slot.
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

    /// Staleness for XTWINOPS, observed through the emitted report.
    #[test]
    fn xtwinops_gate_tracks_the_installed_policy() {
        // `allow_window_ops` stays at its shipping default (false), so the ONLY
        // thing that can open CSI 18 t here is the policy.
        let reports = |term: &mut Terminal| {
            term.process(b"\x1b[18t");
            term.take_response().is_some_and(|r| !r.is_empty())
        };

        let mut term = Terminal::new(24, 80);
        assert!(!reports(&mut term), "no engine, legacy bool false: no report");

        term.apply_policy_engine(PolicyEngine::new(policy_with_rule(
            "CSI t",
            Response::Execute,
        )));
        assert!(
            reports(&mut term),
            "installing `CSI t = Execute` must open the gate immediately"
        );

        term.apply_policy_engine(PolicyEngine::new(policy_with_rule(
            "CSI t",
            Response::Drop,
        )));
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

        term.clear_policy_engine();
        assert!(
            !reports(&mut term),
            "clearing the engine must return the legacy `allow_window_ops` bit \
             (false) to authority"
        );
    }

    /// Every covered `(major, subcommand)` must read back what a live evaluation
    /// says — including under a policy that discriminates BY subcommand, which
    /// is the case a major-only memo would get wrong.
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

    /// A param-sensitive rule must still decide per subcommand. This is the
    /// caveat that makes a major-only memo unsound, pinned as a test.
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

    /// Subcommands outside the compiled range must be evaluated live, not
    /// answered from a slot they do not own.
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
        assert_eq!(state.shell_integration_gate(133, &upper), BridgeDecision::Fallback);

        // An uncovered MAJOR likewise falls through to a live evaluation.
        let mut state = PolicyState::new();
        state.install(PolicyEngine::new(policy_with_rule("OSC 1337", Response::Drop)));
        let a: [&[u8]; 2] = [b"1337", b"A"];
        assert_eq!(state.shell_integration_gate(1337, &a), BridgeDecision::Deny);
    }

    /// An absent subcommand is probed as `"A"` (`probe_subcommand`), and the
    /// compiled table must agree with that.
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

    /// Staleness for the shell-integration gate, observed through the public
    /// drop counter.
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

        term.clear_policy_engine();
        assert!(
            !dropped(&mut term, &mark),
            "clearing the engine leaves the nonce check, which this mark passes"
        );
    }

    #[test]
    fn rate_limit_passthrough_reports_absence_of_an_engine() {
        let mut state = PolicyState::new();
        let clock = aterm_policy::limits::SystemClock;
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

    // -----------------------------------------------------------------
    // The pre-resolved `"response"` limiter slot
    // -----------------------------------------------------------------

    /// A policy declaring the given rate-limit buckets, in the given order,
    /// and no rules. Order matters: it is what decides each id's slot.
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
    fn response_slot_reports_absence_of_an_engine() {
        let mut state = PolicyState::new();
        let clock = aterm_policy::limits::SystemClock;
        assert_eq!(
            state.response_rate_limit_try_consume(1, &clock),
            None,
            "no engine installed: `send_response` must be told so it falls back \
             to the legacy in-transient limiter instead of being allowed outright"
        );
    }

    #[test]
    fn response_slot_answers_exactly_as_the_id_does() {
        let clock = aterm_policy::limits::SystemClock;
        let mut by_id = PolicyState::new();
        let mut by_slot = PolicyState::new();
        by_id.install(PolicyEngine::new(profiles::standard()));
        by_slot.install(PolicyEngine::new(profiles::standard()));
        // Drain the 64 KiB burst in chunks and compare answers at every step,
        // including the ones after the bucket runs dry.
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
        let clock = aterm_policy::limits::SystemClock;
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
        let clock = aterm_policy::limits::SystemClock;
        // `response` is declared SECOND, behind a zero-capacity `palette`
        // bucket that denies everything. A slot that indexed by position
        // instead of by id would debit `palette` and deny every reply.
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

    /// The staleness invariant for the slot, observed end to end through a real
    /// terminal's replies — the same observable the gate-table tests use.
    #[test]
    fn response_slot_tracks_the_installed_policy() {
        let mut term = Terminal::new(24, 80);
        assert!(replies(&mut term), "no engine: the legacy limiter allows");

        // A zero-capacity `response` bucket is a hard block.
        term.apply_policy_engine(PolicyEngine::new(policy_with_limits(&[(
            RESPONSE_LIMIT_ID,
            0,
        )])));
        assert!(
            !replies(&mut term),
            "installing a zero-capacity `response` bucket must suppress the reply \
             immediately"
        );

        // Swap to a policy where the id sits at a DIFFERENT slot and has room.
        term.apply_policy_engine(PolicyEngine::new(policy_with_limits(&[
            ("clipboard", 0),
            (RESPONSE_LIMIT_ID, 64 * 1024),
        ])));
        assert!(
            replies(&mut term),
            "swapping policies must re-resolve the slot; a stale one would still \
             point at the old bucket (and here, at a zero-capacity `clipboard`)"
        );

        // And back the other way, to catch a memo that only ever loosens.
        term.apply_policy_engine(PolicyEngine::new(policy_with_limits(&[(
            RESPONSE_LIMIT_ID,
            0,
        )])));
        assert!(!replies(&mut term), "re-installing the hard block must bite");

        term.clear_policy_engine();
        assert!(
            replies(&mut term),
            "clearing the engine must restore the legacy in-transient limiter, \
             not leave the cleared policy's slot in force"
        );
    }
}
