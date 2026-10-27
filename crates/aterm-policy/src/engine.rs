// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Policy engine: precompiled decision tree + `PolicyEngine::evaluate`.

use std::collections::HashMap as BucketMap;
use std::sync::Arc;

use crate::{
    OriginTag, Policy, RateLimit, Response,
    limits::{RateLimitSlot, RateLimiterSet, TimeSource},
    selector::{BucketKey, DispatchedSequence, FunctionKind, SequenceSelector},
};

/// Result of [`PolicyEngine::evaluate`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Decision {
    /// The effective response for the evaluated sequence.
    pub response: Response,
    /// Zero-based index of the matched rule inside the policy's `rules` vector, or `None` when evaluation fell through to `policy.defaults.unmatched`.
    pub matched_rule: Option<usize>,
    /// Rate-limit id copied from the matched rule (if any).
    pub rate_limit: Option<Arc<str>>,
}

impl Decision {
    /// Build a `Decision` that came from the default path (no rule matched).
    #[must_use]
    #[inline]
    pub(crate) const fn fell_through(response: Response) -> Self {
        Self {
            response,
            matched_rule: None,
            rate_limit: None,
        }
    }
}

/// Precompiled policy with a bucketed decision table.
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
    /// One slot per rule: its rate-limit id, shared into every [`Decision`].
    rate_limit_ids: Vec<Option<Arc<str>>>,
    /// Runtime rate-limiter buckets seeded from `policy.rate_limits`.
    limiters: RateLimiterSet,
}

impl PolicyEngine {
    /// Construct an engine from a [`Policy`].
    #[must_use]
    pub fn new(policy: Policy) -> Self {
        let mut compiled = Vec::with_capacity(policy.rules.len());
        let mut buckets: BucketMap<BucketKey, Vec<usize>> = BucketMap::new();
        let mut wildcard_bucket = Vec::new();
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
                Err(_) => compiled.push(None),
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

    /// Borrow the underlying [`Policy`] — useful for introspection.
    #[must_use]
    #[inline]
    pub const fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Evaluate the policy against a dispatched sequence and its origin.
    #[must_use]
    pub fn evaluate(&self, sequence: &DispatchedSequence, origin: OriginTag) -> Decision {
        let bucket_key = BucketKey {
            function: sequence.function,
            major: sequence.major,
        };
        if let Some(indices) = self.buckets.get(&bucket_key)
            && let Some(decision) = self.walk_bucket(indices, sequence, origin)
        {
            return decision;
        }

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

        if let Some(decision) = self.walk_bucket(&self.wildcard_bucket, sequence, origin) {
            return decision;
        }

        Decision::fell_through(self.policy.defaults.unmatched)
    }

    /// Walk a slice of rule indices in declared order, returning the first
    /// full match (selector + origin gate).
    fn walk_bucket(
        &self,
        indices: &[usize],
        sequence: &DispatchedSequence,
        origin: OriginTag,
    ) -> Option<Decision> {
        for &idx in indices {
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
                rate_limit: self.rate_limit_ids.get(idx).cloned().flatten(),
            });
        }
        None
    }

    /// Attempt to debit `amount` tokens from the named rate-limit bucket.
    #[inline]
    pub fn rate_limit_try_consume<T: TimeSource>(
        &mut self,
        id: &str,
        amount: u64,
        clock: &T,
    ) -> bool {
        self.limiters.try_consume(id, amount, clock)
    }

    /// Resolve a rate-limit id to its [`RateLimitSlot`] in this engine's bucket set — for call sites that debit the same literal id on every dispatch and would otherwise re-hash it each time.
    #[must_use]
    #[inline]
    pub fn rate_limit_slot(&self, id: &str) -> RateLimitSlot {
        self.limiters.slot(id)
    }

    /// Debit a bucket resolved earlier by [`Self::rate_limit_slot`].
    #[inline]
    pub fn rate_limit_try_consume_slot<T: TimeSource>(
        &mut self,
        slot: RateLimitSlot,
        amount: u64,
        clock: &T,
    ) -> bool {
        self.limiters.try_consume_slot(slot, amount, clock)
    }

    /// Look up a [`RateLimit`] configuration by id.
    #[must_use]
    #[inline]
    pub fn rate_limit_config(&self, id: &str) -> Option<&RateLimit> {
        self.policy.rate_limits.iter().find(|cfg| cfg.id == id)
    }

    /// Snapshot every declared rate-limit bucket in first-declaration order.
    #[must_use]
    pub fn bucket_snapshots(&self) -> Vec<crate::limits::BucketSnapshot> {
        self.limiters.snapshots()
    }

    /// Restore declared rate-limit buckets in first-declaration order.
    pub fn restore_bucket_snapshots(&mut self, snaps: &[crate::limits::BucketSnapshot]) {
        self.limiters.restore_snapshots(snaps);
    }

    /// Return the earliest `last_refill` instant across every declared rate-limit bucket.
    #[must_use]
    pub fn earliest_refill(&self) -> Option<aterm_time::Instant> {
        self.limiters.earliest_refill()
    }
}

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

    /// One sequence from one origin under one built-in profile:
    /// `(what, profile, sequence, origin, response, rate-limit id)`.
    type ProfileRow = (
        &'static str,
        fn() -> crate::Policy,
        DispatchedSequence,
        OriginTag,
        Response,
        Option<&'static str>,
    );

    #[test]
    #[rustfmt::skip]
    fn builtin_profile_decisions() {
        use {OriginTag as O, Response as R};
        let permissive: fn() -> crate::Policy = profiles::permissive;
        let standard: fn() -> crate::Policy = profiles::standard;
        let hardened: fn() -> crate::Policy = profiles::hardened;
        let clip_set = || osc(52, &["c", "SGVsbG8="]);
        let clip_query = || osc(52, &["c", "?"]);
        let palette_query = || osc(4, &["3", "?"]);
        let modal = || DispatchedSequence::dcs("2000p");
        let rows: Vec<ProfileRow> = vec![
            ("permissive unmatched", permissive, osc(52, &["c", "SGk="]), O::Pty, R::Execute, None),
            ("permissive window op", permissive, csi(Some(20), 't', &[]), O::Pty, R::Execute, None),
            ("standard clip set, user", standard, clip_set(), O::User, R::Ask,
             Some("clipboard")),
            ("standard clip set, pty", standard, clip_set(), O::Pty, R::Execute, None),
            ("standard clip query, host", standard, clip_query(), O::Host, R::Drop, None),
            ("standard palette query, pty-safe", standard, palette_query(), O::PtySafe, R::Execute,
             None),
            ("standard palette query, pty", standard, palette_query(), O::Pty, R::Execute,
             None),
            ("standard window op, pty", standard, csi(Some(20), 't', &[]), O::Pty, R::Execute,
             None),
            ("standard notification, user", standard, osc(9, &["build done"]), O::User, R::Warn,
             Some("notifications")),
            ("standard dcs 3000p, host", standard, DispatchedSequence::dcs("3000p"), O::Host,
             R::Execute, None),
            ("hardened clip set, user", hardened, clip_set(), O::User, R::Drop, None),
            ("hardened clip set, host", hardened, clip_set(), O::Host, R::Drop, None),
            ("hardened clip query, host", hardened, clip_query(), O::Host, R::Drop, None),
            ("hardened palette query, pty", hardened, palette_query(), O::Pty, R::Drop, None),
            ("hardened palette query, config", hardened, palette_query(), O::ConfigFile, R::Execute,
             None),
            ("hardened modal activation, host", hardened, modal(), O::Host, R::Execute, None),
            ("hardened modal activation, pty", hardened, modal(), O::Pty, R::Drop, None),
            ("hardened unknown, network", hardened, osc(1337, &["leet"]), O::NetworkUntrusted,
             R::Drop, None),
            ("hardened unknown, host", hardened, osc(1337, &["leet"]), O::Host, R::Execute,
             None),
        ];
        for (what, profile, sequence, origin, response, rate_limit) in rows {
            let d = PolicyEngine::new(profile()).evaluate(&sequence, origin);
            assert_eq!(d.response, response, "{what}");
            if rate_limit.is_some() {
                assert_eq!(d.rate_limit.as_deref(), rate_limit, "{what}: rate limit");
            }
        }
    }

    #[test]
    fn malformed_rule_selector_is_skipped() {
        let mut p = profiles::standard();
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
        let d = eng.evaluate(&osc(52, &["c", "SGVsbG8="]), OriginTag::User);
        assert_eq!(d.response, Response::Ask);
    }

    #[test]
    fn origin_gate_failure_does_not_short_circuit() {
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
}
