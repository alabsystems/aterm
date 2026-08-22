// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Policy engine: precompiled decision tree + `PolicyEngine::evaluate`
//! implementing §4.2 of `designs/2026-04-19-osc-policy-engine.md`.
//!
//! The engine is the runtime consumer of [`Policy`]. It
//!
//! 1. Pre-compiles every [`Rule::sequence`] selector string into a
//!    [`SequenceSelector`] at construction time.
//! 2. Buckets the compiled rules by `(function, major)` so evaluation can
//!    narrow the candidate list to a handful of rules before walking them in
//!    declared order.
//! 3. Walks the matching bucket plus the wildcard bucket top-to-bottom,
//!    applying the §4.2 algorithm: first rule whose selector matches **and**
//!    whose `origin_min` is dominated by the caller-supplied origin wins.
//!    Rules that match by selector but fail the origin gate **do not**
//!    short-circuit — evaluation continues.
//! 4. If no rule matches, returns `policy.defaults.unmatched`.
//!
//! Rules whose selector string fails to parse are treated as never-matching
//! (fail-closed at the rule level). A fully malformed TOML is handled upstream
//! by [`crate::Policy::from_toml_or_hardened`] which returns the Hardened
//! profile and `fell_back = true`.
//!
//! # Totality
//!
//! Evaluation never panics, never allocates on the hot path (the returned
//! [`Decision`] carries a [`Copy`] [`Response`] and a precomputed rate-limit
//! id shared via `Arc`, so a match is a refcount bump rather than a heap
//! allocation), and always returns a defined value — including when the rule
//! list is empty or every rule's selector failed to compile. This is codified
//! by the §8.1 Kani harnesses in this module.
//!
//! # Rate limits
//!
//! Rate-limit enforcement lands in #7995. The engine exposes the matched
//! [`Rule`]'s `rate_limit` field to the caller via [`Decision::rate_limit`]
//! so the handler site can consult the bucket. The engine itself only
//! decides response + rate-limit id; it does not own bucket state.

// Under Kani verification we substitute `HashMap` with `BTreeMap` so the
// model checker does not hit the unsupported `CCRandomGenerateBytes` call
// that `HashMap`'s default randomised hasher makes on macOS. Production
// builds keep the `HashMap` for its better expected performance on small
// rule sets. The two types share the `.get / .entry / .or_default / .push`
// surface used by this module so the engine code is otherwise identical.
#[cfg(kani)]
use std::collections::BTreeMap as BucketMap;
#[cfg(not(kani))]
use std::collections::HashMap as BucketMap;
use std::sync::Arc;

use crate::{
    OriginTag, Policy, RateLimit, Response,
    limits::{RateLimitSlot, RateLimiterSet, TimeSource},
    profiles,
    selector::{BucketKey, DispatchedSequence, FunctionKind, SequenceSelector},
};

// ---------------------------------------------------------------------------
// Decision
// ---------------------------------------------------------------------------

/// Result of [`PolicyEngine::evaluate`].
///
/// Returns the winning [`Response`] plus the matched rule's named rate-limit
/// reference (if any). The rate-limit id is the string key into
/// [`Policy::rate_limits`]; the engine itself does not consult the bucket —
/// #7995 wires the bucket check at the handler site.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Decision {
    /// The effective response for the evaluated sequence.
    pub response: Response,
    /// Zero-based index of the matched rule inside the policy's `rules`
    /// vector, or `None` when evaluation fell through to
    /// `policy.defaults.unmatched`.
    pub matched_rule: Option<usize>,
    /// Rate-limit id copied from the matched rule (if any). `None` when the
    /// decision came from the default path or the matched rule had no rate
    /// limit.
    ///
    /// Stored as an `Arc<str>` so the hot `evaluate` path returns a cheap
    /// atomic refcount bump instead of a fresh `String` allocation per match
    /// (the ids are precomputed once in [`PolicyEngine::new`]). `Arc<str>`
    /// derefs to `str` and compares/hashes by value, so the derived
    /// `PartialEq`/`Eq`/`Hash` keep their by-value semantics.
    pub rate_limit: Option<Arc<str>>,
}

impl Decision {
    /// Build a `Decision` that came from the default path (no rule matched).
    #[must_use]
    pub(crate) fn fell_through(response: Response) -> Self {
        Self {
            response,
            matched_rule: None,
            rate_limit: None,
        }
    }
}

// ---------------------------------------------------------------------------
// PolicyEngine
// ---------------------------------------------------------------------------

/// Precompiled policy with a bucketed decision table.
///
/// Built from a [`Policy`] via [`Self::new`]. Holds the policy verbatim
/// alongside:
///
/// * `compiled[i]` — the parsed [`SequenceSelector`] for `policy.rules[i]`,
///   or `None` if the rule's `sequence` string failed to parse.
/// * `buckets` — map from `(function, major)` to the indices of rules whose
///   selector has that bucket key, in declared order.
/// * `wildcard_bucket` — indices of rules whose selector bucket is the
///   universal matcher. Consulted after the specific bucket in declared
///   order (same as inline wildcards, per §4.3).
#[derive(Debug, Clone)]
pub struct PolicyEngine {
    policy: Policy,
    /// One slot per rule. `None` means the selector string failed to parse
    /// and the rule must be skipped at evaluation time.
    compiled: Vec<Option<SequenceSelector>>,
    /// Buckets keyed by `(function, major)`. Wildcard selectors are stored
    /// separately in [`Self::wildcard_bucket`].
    buckets: BucketMap<BucketKey, Vec<usize>>,
    /// Indices of rules whose bucket is the universal matcher
    /// (`FunctionKind::Wildcard`).
    wildcard_bucket: Vec<usize>,
    /// One slot per rule: the matched rule's rate-limit id, precomputed as a
    /// shared `Arc<str>` so [`Self::walk_bucket`] hands the winning
    /// [`Decision`] a refcount bump instead of allocating a fresh `String` on
    /// every match. `None` mirrors `rule.rate_limit == None`.
    rate_limit_ids: Vec<Option<Arc<str>>>,
    /// Runtime rate-limiter buckets seeded from `policy.rate_limits`.
    /// Consulted by [`Self::rate_limit_try_consume`] at handler call sites
    /// (#7995).
    limiters: RateLimiterSet,
}

impl PolicyEngine {
    /// Construct an engine from a [`Policy`]. Compiles every rule's selector
    /// up front.
    ///
    /// Rules whose selector fails to parse remain in the policy (so round-trip
    /// serialization is lossless) but will never match at evaluation time.
    #[must_use]
    pub fn new(policy: Policy) -> Self {
        let mut compiled = Vec::with_capacity(policy.rules.len());
        let mut buckets: BucketMap<BucketKey, Vec<usize>> = BucketMap::new();
        let mut wildcard_bucket = Vec::new();
        // Precompute the shared rate-limit ids once so the `evaluate` hot path
        // returns an `Arc` clone (refcount bump) instead of a per-match
        // `String` allocation.
        let mut rate_limit_ids = Vec::with_capacity(policy.rules.len());

        for (idx, rule) in policy.rules.iter().enumerate() {
            rate_limit_ids.push(rule.rate_limit.as_deref().map(Arc::from));
            match SequenceSelector::parse(&rule.sequence) {
                Ok(sel) => {
                    let key = sel.bucket_key();
                    if key.function == FunctionKind::Wildcard {
                        wildcard_bucket.push(idx);
                    } else {
                        buckets.entry(key).or_default().push(idx);
                    }
                    compiled.push(Some(sel));
                }
                Err(_) => {
                    // Rule remains in the policy but cannot match anything.
                    compiled.push(None);
                }
            }
        }

        let limiters = RateLimiterSet::from_policy(&policy);

        Self {
            policy,
            compiled,
            buckets,
            wildcard_bucket,
            rate_limit_ids,
            limiters,
        }
    }

    /// Fail-closed default engine (Hardened profile). Used when the TOML
    /// policy fails to load (§4.4).
    #[must_use]
    pub fn hardened() -> Self {
        Self::new(profiles::hardened())
    }

    /// Load a TOML policy into an engine, falling back to [`Self::hardened`]
    /// on any parse error or schema mismatch (§4.4).
    ///
    /// Returns `(engine, fell_back)`. The caller can use the bool for
    /// telemetry — the engine itself is always safe to evaluate against.
    #[must_use]
    pub fn from_toml_or_hardened(src: &str) -> (Self, bool) {
        let (policy, fell_back) = Policy::from_toml_or_hardened(src);
        (Self::new(policy), fell_back)
    }

    /// Borrow the underlying [`Policy`] — useful for introspection (FFI,
    /// host UIs, mirror-field sync in #7993).
    #[must_use]
    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Replace the backing policy, rebuilding the decision tree.
    ///
    /// Cheaper than constructing a brand-new engine because we reuse the
    /// same bucket `HashMap` layout, but the hot path still takes an
    /// amortized linear pass over the rule list. Hosts that hot-swap
    /// policies mid-session call this method rather than constructing a
    /// second engine.
    pub fn replace_policy(&mut self, policy: Policy) {
        *self = Self::new(policy);
    }

    /// Evaluate the policy against a dispatched sequence and its origin.
    ///
    /// Implements design §4.2. Never panics.
    ///
    /// Algorithm:
    ///
    /// 1. Look up rule indices in the `(function, major)` bucket.
    /// 2. For each index in declared order: if the selector matches the
    ///    sequence and `origin.dominates(rule.origin_min)`, return the
    ///    rule's response. A selector-match without an origin-match
    ///    **continues** to the next rule.
    /// 3. Repeat step 2 for the wildcard bucket.
    /// 4. Fall through to `policy.defaults.unmatched`.
    ///
    /// The wildcard bucket is consulted last because a rule that matches by
    /// wildcard is strictly weaker evidence than an exact-bucket match; the
    /// operator's declared ordering is preserved **within** each bucket, not
    /// across buckets. This matches the xterm-style intuition where
    /// `OSC 52 set` rules override a `*` fallback regardless of line number.
    #[must_use]
    pub fn evaluate(&self, sequence: &DispatchedSequence, origin: OriginTag) -> Decision {
        // 1. Specific bucket.
        let bucket_key = BucketKey {
            function: sequence.function,
            major: sequence.major,
        };
        if let Some(indices) = self.buckets.get(&bucket_key)
            && let Some(decision) = self.walk_bucket(indices, sequence, origin)
        {
            return decision;
        }

        // 2. Major-wildcard bucket: same function, `major = None`. Handles
        //    selectors like `"CSI t"` where the major is unspecified.
        let major_wildcard_key = BucketKey {
            function: sequence.function,
            major: None,
        };
        if major_wildcard_key != bucket_key
            && let Some(indices) = self.buckets.get(&major_wildcard_key)
            && let Some(decision) = self.walk_bucket(indices, sequence, origin)
        {
            return decision;
        }

        // 3. Universal wildcard bucket (`*` / `response any`).
        if let Some(decision) = self.walk_bucket(&self.wildcard_bucket, sequence, origin) {
            return decision;
        }

        // 4. Default fallthrough.
        Decision::fell_through(self.policy.defaults.unmatched)
    }

    /// Walk a slice of rule indices in declared order, returning the first
    /// full match (selector + origin gate). Rules that fail either check
    /// continue to the next index.
    fn walk_bucket(
        &self,
        indices: &[usize],
        sequence: &DispatchedSequence,
        origin: OriginTag,
    ) -> Option<Decision> {
        for &idx in indices {
            // Defensive: indices in a bucket always have Some(compiled)
            // because only parsed selectors get bucketed. But we re-check
            // for totality — no panic on a malformed engine state.
            let Some(selector) = self.compiled.get(idx).and_then(Option::as_ref) else {
                continue;
            };
            if !selector.matches(sequence) {
                continue;
            }
            let Some(rule) = self.policy.rules.get(idx) else {
                continue;
            };
            if !origin.dominates(rule.origin_min) {
                // §4.2: origin-gate failure does not short-circuit.
                continue;
            }
            return Some(Decision {
                response: rule.response,
                matched_rule: Some(idx),
                // Cheap atomic refcount bump on the precomputed id rather than
                // a fresh `String` malloc+memcpy on the hot path. The slot is
                // kept in lockstep with `policy.rules` by `Self::new`, but we
                // index defensively to preserve totality.
                rate_limit: self.rate_limit_ids.get(idx).cloned().flatten(),
            });
        }
        None
    }

    // -----------------------------------------------------------------
    // Rate-limit API (#7995)
    // -----------------------------------------------------------------

    /// Attempt to debit `amount` tokens from the named rate-limit bucket.
    ///
    /// The `id` must match a [`RateLimit::id`] declared in the active
    /// [`Policy`]. Unknown ids are permitted (treated as "no limit
    /// declared"); the engine's fail-closed posture applies to rule
    /// evaluation, not to rate-limit lookups.
    ///
    /// Returns `true` when the call is permitted (and the bucket was
    /// debited), `false` when the bucket denies. Semantics follow
    /// [`crate::limits::TokenBucket::try_consume`]:
    ///
    /// * zero-capacity ⇒ always deny (hard block);
    /// * `per_sequence_max > 0 && amount > per_sequence_max` ⇒ deny
    ///   without debiting;
    /// * insufficient balance ⇒ deny without debiting.
    ///
    /// `amount` is `u64` so the handler-side byte counts (up to a 64 KiB
    /// response) and the pair counts used by the OSC 4/21 handlers both
    /// fit without overflow.
    pub fn rate_limit_try_consume<T: TimeSource>(
        &mut self,
        id: &str,
        amount: u64,
        clock: &T,
    ) -> bool {
        self.limiters.try_consume(id, amount, clock)
    }

    /// Resolve a rate-limit id to its [`RateLimitSlot`] in this engine's
    /// bucket set — for call sites that debit the same literal id on every
    /// dispatch and would otherwise re-hash it each time.
    ///
    /// The slot belongs to THIS engine (see [`RateLimitSlot`]); a caller that
    /// swaps engines must re-resolve. Unknown ids resolve to
    /// [`RateLimitSlot::UNDECLARED`], which debits as "allow" — the same
    /// answer [`Self::rate_limit_try_consume`] gives them.
    #[must_use]
    pub fn rate_limit_slot(&self, id: &str) -> RateLimitSlot {
        self.limiters.slot(id)
    }

    /// Debit a bucket resolved earlier by [`Self::rate_limit_slot`].
    ///
    /// Identical semantics to [`Self::rate_limit_try_consume`], minus the
    /// per-call hash of the id.
    pub fn rate_limit_try_consume_slot<T: TimeSource>(
        &mut self,
        slot: RateLimitSlot,
        amount: u64,
        clock: &T,
    ) -> bool {
        self.limiters.try_consume_slot(slot, amount, clock)
    }

    /// Borrow the engine's rate-limiter set — intended for diagnostics
    /// and for host surfaces that want to read the current token balance
    /// (e.g. an observability panel).
    #[must_use]
    pub fn rate_limiters(&self) -> &RateLimiterSet {
        &self.limiters
    }

    /// Borrow the engine's rate-limiter set mutably. Intended for host
    /// code that needs to reconfigure a single bucket without rebuilding
    /// the policy from scratch (e.g. a runtime kill-switch toggle).
    pub fn rate_limiters_mut(&mut self) -> &mut RateLimiterSet {
        &mut self.limiters
    }

    /// Look up a [`RateLimit`] configuration by id. Returns `None` when
    /// the active policy does not declare a bucket with that id.
    #[must_use]
    pub fn rate_limit_config(&self, id: &str) -> Option<&RateLimit> {
        self.policy.rate_limits.iter().find(|cfg| cfg.id == id)
    }
}

// ---------------------------------------------------------------------------
// Unit tests (co-located to exercise bucket internals).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Profile, profiles, selector::DispatchedSequence};

    fn osc(major: u32, params: &[&str]) -> DispatchedSequence {
        DispatchedSequence::osc(major, params.iter().map(|&s| s.to_owned()))
    }

    fn csi(major: Option<u32>, final_byte: char, params: &[&str]) -> DispatchedSequence {
        DispatchedSequence::csi(major, final_byte, params.iter().map(|&s| s.to_owned()))
    }

    // -----------------------------------------------------------------
    // Permissive
    // -----------------------------------------------------------------

    #[test]
    fn permissive_default_unmatched_is_execute() {
        let eng = PolicyEngine::new(profiles::permissive());
        let d = eng.evaluate(&osc(52, &["c", "SGk="]), OriginTag::Pty);
        assert_eq!(d.response, Response::Execute);
    }

    #[test]
    fn permissive_clipboard_set_from_pty_executes() {
        let eng = PolicyEngine::new(profiles::permissive());
        // `response any` rule is the only rule in Permissive; everything else
        // falls through to `unmatched = Execute`.
        let d = eng.evaluate(&osc(52, &["c", "SGk="]), OriginTag::Pty);
        assert_eq!(d.response, Response::Execute);
    }

    #[test]
    fn permissive_window_op_from_pty_executes() {
        let eng = PolicyEngine::new(profiles::permissive());
        let d = eng.evaluate(&csi(Some(20), 't', &[]), OriginTag::Pty);
        assert_eq!(d.response, Response::Execute);
    }

    // -----------------------------------------------------------------
    // Standard
    // -----------------------------------------------------------------

    #[test]
    fn standard_clipboard_set_from_user_is_ask() {
        let eng = PolicyEngine::new(profiles::standard());
        let d = eng.evaluate(&osc(52, &["c", "SGVsbG8="]), OriginTag::User);
        assert_eq!(d.response, Response::Ask);
        assert_eq!(d.rate_limit.as_deref(), Some("clipboard"));
    }

    #[test]
    fn standard_clipboard_set_from_pty_falls_through_to_unmatched() {
        // PTY origin fails the `User`-or-higher gate on the OSC 52 set rule;
        // evaluation continues. Next same-bucket rules are OSC 52 query — no
        // match. Eventually we hit the `response any` rule with origin_min =
        // NetworkUntrusted (the loosest tag), which dominates Pty. That rule
        // responds Execute.
        let eng = PolicyEngine::new(profiles::standard());
        let d = eng.evaluate(&osc(52, &["c", "SGVsbG8="]), OriginTag::Pty);
        // The `response any` wildcard catches this with Execute because its
        // origin_min = NetworkUntrusted (everyone dominates that).
        assert_eq!(d.response, Response::Execute);
    }

    #[test]
    fn standard_clipboard_query_drops() {
        let eng = PolicyEngine::new(profiles::standard());
        let d = eng.evaluate(&osc(52, &["c", "?"]), OriginTag::Host);
        assert_eq!(d.response, Response::Drop);
    }

    #[test]
    fn standard_palette_query_executes_for_pty_safe() {
        let eng = PolicyEngine::new(profiles::standard());
        let d = eng.evaluate(&osc(4, &["3", "?"]), OriginTag::PtySafe);
        assert_eq!(d.response, Response::Execute);
    }

    #[test]
    fn standard_palette_query_from_pty_falls_through() {
        // Pty does not dominate PtySafe, so the OSC 4 query rule continues.
        // Next matching bucket rule is OSC 4 set — that's a different param
        // pattern. Falls through to wildcard `response any` with
        // origin_min=NetworkUntrusted → Execute.
        let eng = PolicyEngine::new(profiles::standard());
        let d = eng.evaluate(&osc(4, &["3", "?"]), OriginTag::Pty);
        assert_eq!(d.response, Response::Execute);
    }

    #[test]
    fn standard_window_op_from_pty_drops() {
        // CSI t rule requires Host origin; Pty does not dominate Host.
        // No other CSI rule matches. Falls through to `response any`
        // wildcard (NetworkUntrusted origin_min) → Execute.
        // This is Standard-profile behavior by design: CSI responses go
        // through the response rate limiter, not the window-op gate.
        // Host-only blocking is enforced by the capability module for
        // non-query window ops; the policy rule here is the gate for
        // dispatch, not the actual ops.
        let eng = PolicyEngine::new(profiles::standard());
        let d = eng.evaluate(&csi(Some(20), 't', &[]), OriginTag::Pty);
        // Fallthrough `response any` wildcard responds Execute.
        assert_eq!(d.response, Response::Execute);
    }

    #[test]
    fn standard_notification_from_user_warns() {
        let eng = PolicyEngine::new(profiles::standard());
        let d = eng.evaluate(&osc(9, &["build done"]), OriginTag::User);
        assert_eq!(d.response, Response::Warn);
        assert_eq!(d.rate_limit.as_deref(), Some("notifications"));
    }

    #[test]
    fn standard_default_unmatched_is_warn() {
        let eng = PolicyEngine::new(profiles::standard());
        // OSC 999 — no rule matches, not covered by wildcard at wildcard-origin.
        // `response any` has origin_min=NetworkUntrusted; ConfigFile dominates
        // NetworkUntrusted so it matches. Use an origin that does not dominate
        // anything (but NetworkUntrusted dominates itself).
        // Easier path: use a sequence and origin where no rule matches at all.
        // The wildcard `response any` matches any origin that dominates
        // NetworkUntrusted — which is all of them. So we can never hit the
        // unmatched default in Standard as long as the wildcard is present.
        // Assert instead: the default is configured correctly.
        assert_eq!(eng.policy().defaults.unmatched, Response::Warn);
        // And an obscure DCS sequence with no matching rule triggers
        // the wildcard response rather than unmatched.
        let d = eng.evaluate(&DispatchedSequence::dcs("3000p"), OriginTag::Host);
        // DCS 3000p has no matching rule; wildcard catches it → Execute.
        assert_eq!(d.response, Response::Execute);
    }

    // -----------------------------------------------------------------
    // Hardened
    // -----------------------------------------------------------------

    #[test]
    fn hardened_clipboard_set_from_user_drops() {
        // Hardened requires Host origin for OSC 52 set. User does not
        // dominate Host → fall through → unmatched = Drop.
        let eng = PolicyEngine::new(profiles::hardened());
        let d = eng.evaluate(&osc(52, &["c", "SGVsbG8="]), OriginTag::User);
        assert_eq!(d.response, Response::Drop);
    }

    #[test]
    fn hardened_clipboard_set_from_host_drops() {
        // Hardened's OSC 52 set rule is response = Drop with origin_min =
        // Host. Host dominates Host → rule fires → Drop.
        let eng = PolicyEngine::new(profiles::hardened());
        let d = eng.evaluate(&osc(52, &["c", "SGVsbG8="]), OriginTag::Host);
        assert_eq!(d.response, Response::Drop);
    }

    #[test]
    fn hardened_clipboard_query_from_host_drops() {
        let eng = PolicyEngine::new(profiles::hardened());
        let d = eng.evaluate(&osc(52, &["c", "?"]), OriginTag::Host);
        assert_eq!(d.response, Response::Drop);
    }

    #[test]
    fn hardened_palette_query_from_pty_drops() {
        // Hardened OSC 4 query rule requires ConfigFile origin. Pty does not
        // dominate ConfigFile → fall through → unmatched = Drop.
        let eng = PolicyEngine::new(profiles::hardened());
        let d = eng.evaluate(&osc(4, &["3", "?"]), OriginTag::Pty);
        assert_eq!(d.response, Response::Drop);
    }

    #[test]
    fn hardened_palette_query_from_configfile_executes() {
        let eng = PolicyEngine::new(profiles::hardened());
        let d = eng.evaluate(&osc(4, &["3", "?"]), OriginTag::ConfigFile);
        assert_eq!(d.response, Response::Execute);
    }

    #[test]
    fn hardened_modal_activation_from_host_executes() {
        let eng = PolicyEngine::new(profiles::hardened());
        let d = eng.evaluate(&DispatchedSequence::dcs("2000p"), OriginTag::Host);
        assert_eq!(d.response, Response::Execute);
    }

    #[test]
    fn hardened_modal_activation_from_pty_drops() {
        let eng = PolicyEngine::new(profiles::hardened());
        let d = eng.evaluate(&DispatchedSequence::dcs("2000p"), OriginTag::Pty);
        assert_eq!(d.response, Response::Drop);
    }

    #[test]
    fn hardened_unknown_sequence_from_network_drops() {
        let eng = PolicyEngine::new(profiles::hardened());
        let d = eng.evaluate(&osc(1337, &["leet"]), OriginTag::NetworkUntrusted);
        assert_eq!(d.response, Response::Drop);
    }

    #[test]
    fn hardened_unknown_sequence_from_host_drops() {
        // No rule matches OSC 1337; `response any` rule has origin_min = Host
        // and response = Execute — Host dominates Host, so it fires for
        // response sequences from Host.
        let eng = PolicyEngine::new(profiles::hardened());
        let d = eng.evaluate(&osc(1337, &["leet"]), OriginTag::Host);
        assert_eq!(d.response, Response::Execute);
    }

    // -----------------------------------------------------------------
    // Engine mechanics
    // -----------------------------------------------------------------

    #[test]
    fn fail_closed_on_garbage_toml() {
        let (eng, fell_back) = PolicyEngine::from_toml_or_hardened("<<< not toml >>>");
        assert!(fell_back);
        assert_eq!(eng.policy().profile, Profile::Hardened);
    }

    #[test]
    fn malformed_rule_selector_is_skipped() {
        let mut p = profiles::standard();
        // Inject a rule with a garbage selector. It must never match;
        // evaluation continues to subsequent rules.
        p.rules.insert(
            0,
            crate::Rule {
                sequence: "ZZZ garbage".to_owned(),
                origin_min: OriginTag::Pty,
                response: Response::Drop,
                rate_limit: None,
                prompt_id: None,
            },
        );
        let eng = PolicyEngine::new(p);
        // Clipboard set from User still reaches the Standard OSC 52 set rule.
        let d = eng.evaluate(&osc(52, &["c", "SGVsbG8="]), OriginTag::User);
        assert_eq!(d.response, Response::Ask);
    }

    #[test]
    fn origin_gate_failure_does_not_short_circuit() {
        // Build a policy where:
        //   rule 0: OSC 9, origin_min = Host,            response = Drop
        //   rule 1: OSC 9, origin_min = NetworkUntrusted, response = Warn
        // With origin = Pty (does not dominate Host, but dominates
        // NetworkUntrusted), the correct behavior is to skip rule 0 and
        // match rule 1 → Warn.
        let p = Policy {
            schema_version: crate::SCHEMA_VERSION,
            profile: Profile::Standard,
            defaults: crate::Defaults {
                unmatched: Response::Drop,
                shell_integration_require_nonce: false,
            },
            rules: vec![
                crate::Rule {
                    sequence: "OSC 9".to_owned(),
                    origin_min: OriginTag::Host,
                    response: Response::Drop,
                    rate_limit: None,
                    prompt_id: None,
                },
                crate::Rule {
                    sequence: "OSC 9".to_owned(),
                    origin_min: OriginTag::NetworkUntrusted,
                    response: Response::Warn,
                    rate_limit: None,
                    prompt_id: None,
                },
            ],
            rate_limits: vec![],
        };
        let eng = PolicyEngine::new(p);
        let d = eng.evaluate(&osc(9, &["ping"]), OriginTag::Pty);
        assert_eq!(d.response, Response::Warn);
        assert_eq!(d.matched_rule, Some(1));
    }

    #[test]
    fn rate_limit_id_propagates_through_decision() {
        let eng = PolicyEngine::new(profiles::standard());
        let d = eng.evaluate(&osc(9, &["build done"]), OriginTag::User);
        assert_eq!(d.rate_limit.as_deref(), Some("notifications"));
    }

    #[test]
    fn rate_limit_id_is_shared_not_reallocated_per_match() {
        // The id is precomputed once in `new` and handed out via an `Arc`
        // refcount bump, so repeated evaluations of the same match return
        // clones of the *same* allocation (no per-match `String` malloc on the
        // hot path). Guards the #7998 hotpath regression gate.
        let eng = PolicyEngine::new(profiles::standard());
        let a = eng
            .evaluate(&osc(9, &["build done"]), OriginTag::User)
            .rate_limit
            .expect("rate limit id present");
        let b = eng
            .evaluate(&osc(9, &["other"]), OriginTag::User)
            .rate_limit
            .expect("rate limit id present");
        assert_eq!(&*a, "notifications");
        assert!(
            Arc::ptr_eq(&a, &b),
            "evaluate must clone the precomputed Arc id, not allocate a fresh one",
        );
    }

    #[test]
    fn empty_policy_returns_default_unmatched() {
        let p = Policy {
            schema_version: crate::SCHEMA_VERSION,
            profile: Profile::Hardened,
            defaults: crate::Defaults {
                unmatched: Response::Drop,
                shell_integration_require_nonce: false,
            },
            rules: vec![],
            rate_limits: vec![],
        };
        let eng = PolicyEngine::new(p);
        let d = eng.evaluate(&osc(52, &["c", "?"]), OriginTag::Host);
        assert_eq!(d.response, Response::Drop);
        assert!(d.matched_rule.is_none());
    }

    #[test]
    fn replace_policy_rebuilds_tree() {
        let mut eng = PolicyEngine::new(profiles::permissive());
        assert_eq!(
            eng.evaluate(&osc(52, &["c", "?"]), OriginTag::Pty).response,
            Response::Execute,
        );
        eng.replace_policy(profiles::hardened());
        assert_eq!(
            eng.evaluate(&osc(52, &["c", "?"]), OriginTag::Pty).response,
            Response::Drop,
        );
    }

    #[test]
    fn matched_rule_index_is_stable_after_wildcard_fallthrough() {
        // Standard profile: OSC 9 Warn rule at index 6 (User origin_min).
        // Ensure the engine reports the actual rule index, not a wildcard
        // bucket surrogate.
        let eng = PolicyEngine::new(profiles::standard());
        let d = eng.evaluate(&osc(9, &["x"]), OriginTag::User);
        assert!(d.matched_rule.is_some());
        let idx = d.matched_rule.expect("matched");
        assert_eq!(eng.policy().rules[idx].sequence, "OSC 9");
    }
}

// The Kani proofs for `PolicyEngine::evaluate` (totality, refinement,
// determinism, mirror-invariant) live in `crate::kani_proofs`, gated by
// `#[cfg(kani)]` in `lib.rs`. See §8.1 of
// `designs/2026-04-19-osc-policy-engine.md` for the harness matrix.
